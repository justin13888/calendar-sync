//! The HTTP shapes the seam produces and consumes. No I/O happens here: the application sends a
//! [`HttpRequest`] with its own client and hands back what it got.

use core::fmt;

/// Where every request goes: the Graph service root, without the trailing `/`. A request's
/// [`HttpRequest::path`] follows it.
pub const BASE_URL: &str = "https://graph.microsoft.com";

/// An HTTP method the seam uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`: a delta page, an event, or a lookup.
    Get,
    /// `POST`: a create.
    Post,
    /// `PATCH`: an update of the properties that changed.
    Patch,
    /// `DELETE`: a delete.
    Delete,
}

impl Method {
    /// The method's name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request ready to send. It carries no credentials: the transport adds the `Authorization`
/// header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method.
    pub method: Method,
    /// The path and query, starting `/v1.0/`, with every parameter percent-encoded. The URL is
    /// [`BASE_URL`] followed by it. A delta link Graph returned on another host is kept whole.
    pub path: String,
    /// The headers the request needs: `Prefer` on every request, `If-Match` on a conditional
    /// write where the seam sends it, and `Content-Type` with a body.
    pub headers: Vec<(String, String)>,
    /// The JSON body, on a create or an update.
    pub body: Option<String>,
}

impl HttpRequest {
    /// The full URL: [`BASE_URL`] and [`Self::path`], or the path itself where it is already a
    /// URL.
    #[must_use]
    pub fn url(&self) -> String {
        if self.path.starts_with('/') {
            format!("{BASE_URL}{}", self.path)
        } else {
            self.path.clone()
        }
    }

    /// The value of the header `name`, compared without regard to case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// A response as received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The headers. The seam reads none of them: they are kept for the application, which may
    /// honour `Retry-After` before the engine tries a throttled request again.
    pub headers: Vec<(String, String)>,
    /// The body's bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response with this status and body, and no headers.
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// The value of the header `name`, compared without regard to case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// A request that produced no response: the connection failed, timed out, or the response was
/// lost. The request may or may not have taken effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    /// What went wrong, for a log.
    pub message: String,
}

impl TransportError {
    /// A transport error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "transport error: {}", self.message)
    }
}

impl core::error::Error for TransportError {}

/// Sends requests. The application implements it over its HTTP client, adding the credentials;
/// a test implements it over a fake or a recorded fixture.
pub trait Transport {
    /// Sends `request` and returns the response, whatever its status.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when no response arrived.
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        (**self).send(request)
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Percent-encodes `text` for a path segment or a query value: every byte but the RFC 3986
/// unreserved characters.
#[must_use]
pub fn encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{HttpRequest, Method, encode};

    #[test]
    fn encodes_everything_but_unreserved() {
        assert_eq!(encode("a-Z_0.9~"), "a-Z_0.9~");
        assert_eq!(encode("AAMkAGI2TGuLAAA="), "AAMkAGI2TGuLAAA%3D");
        assert_eq!(encode("ep/id eq 'x'"), "ep%2Fid%20eq%20%27x%27");
        assert_eq!(encode("é"), "%C3%A9");
    }

    #[test]
    fn a_path_is_joined_and_a_url_kept() {
        let mut request = HttpRequest {
            method: Method::Get,
            path: "/v1.0/me/events/a".to_owned(),
            headers: Vec::new(),
            body: None,
        };
        assert_eq!(
            request.url(),
            "https://graph.microsoft.com/v1.0/me/events/a"
        );
        request.path = "https://graph.example.invalid/v1.0/me/calendarView/delta".to_owned();
        assert_eq!(
            request.url(),
            "https://graph.example.invalid/v1.0/me/calendarView/delta"
        );
    }
}

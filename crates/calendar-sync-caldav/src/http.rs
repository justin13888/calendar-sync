//! The HTTP shapes the seam produces and consumes. No I/O happens here: the application sends a
//! [`HttpRequest`] with its own client and hands back what it got.

use core::fmt;

/// A request ready to send. It carries no credentials: the transport adds the `Authorization`
/// header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method: `REPORT`, `PROPFIND`, `OPTIONS`, `GET`, `PUT` or `DELETE`.
    pub method: String,
    /// The absolute URL: the server's base URL and the percent-encoded path.
    pub url: String,
    /// The headers the request needs: `Depth` and `Content-Type` on a report or a `PROPFIND`,
    /// `If-Match` or `If-None-Match` on a write.
    pub headers: Vec<(String, String)>,
    /// The body: XML on a report or a `PROPFIND`, iCalendar on a `PUT`.
    pub body: Option<String>,
}

impl HttpRequest {
    /// The path and query of [`Self::url`], which is what a recorded fixture names.
    #[must_use]
    pub fn path(&self) -> &str {
        let after_scheme = self
            .url
            .find("://")
            .map_or(self.url.as_str(), |index| &self.url[index + 3..]);
        after_scheme
            .find('/')
            .map_or("/", |index| &after_scheme[index..])
    }

    /// The value of the header `name`, compared without regard to case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The request `libdav` built.
    pub(crate) fn from_http(request: &http::Request<String>) -> Self {
        let body = request.body();
        Self {
            method: request.method().as_str().to_owned(),
            url: request.uri().to_string(),
            headers: request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: (!body.is_empty()).then(|| body.clone()),
        }
    }
}

/// A response as received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The headers. The seam reads `ETag`, `DAV` and `Schedule-Tag`.
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

    /// The response with the header `name` added.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The value of the header `name`, compared without regard to case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The response as `libdav` parses it: its status and headers, and the body apart.
    pub(crate) fn parts(&self) -> http::response::Parts {
        let mut builder = http::Response::builder().status(self.status);
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder.body(()).unwrap_or_else(|_| {
            // A status outside 100-999 or an unencodable header: keep only what parses.
            let mut response = http::Response::new(());
            *response.status_mut() = http::StatusCode::from_u16(self.status)
                .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
            response
        });
        response.into_parts().0
    }

    /// Whether the status is `2xx`.
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
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
/// a test implements it over a recorded fixture.
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

#[cfg(test)]
mod tests {
    use super::{HttpRequest, HttpResponse};

    #[test]
    fn the_path_is_what_follows_the_authority() {
        let mut request = HttpRequest {
            method: "GET".to_owned(),
            url: "https://dav.example.com/cal/a.ics?x=1".to_owned(),
            headers: vec![("If-Match".to_owned(), "\"1\"".to_owned())],
            body: None,
        };
        assert_eq!(request.path(), "/cal/a.ics?x=1");
        assert_eq!(request.header("if-match"), Some("\"1\""));
        request.url = "https://dav.example.com".to_owned();
        assert_eq!(request.path(), "/");
    }

    #[test]
    fn a_response_keeps_its_status_and_headers_for_libdav() {
        let response = HttpResponse::new(207, "").with_header("ETag", "\"2\"");
        let parts = response.parts();
        assert_eq!(parts.status.as_u16(), 207);
        assert_eq!(parts.headers.get("etag").unwrap(), "\"2\"");
        assert_eq!(response.header("etag"), Some("\"2\""));
    }
}

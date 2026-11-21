#!/bin/sh
# Starts a throwaway CalDAV server in a container, provisions an account and a calendar, then runs
# COMMAND with the variables the calendar-sync-caldav live suite reads:
#
#   sh tests/fixtures/caldav/serve.sh rustical|stalwart COMMAND...
#
# e.g. `sh tests/fixtures/caldav/serve.sh rustical cargo test -p calendar-sync-caldav --test live
# -- --ignored`. RustiCal and Stalwart are AGPL-licensed: they run only as separate processes, in
# their published images, and no code of theirs enters this repository. Each image is pinned by
# digest. The server listens on 127.0.0.1 only and its credentials never leave this script's
# environment.
#
# CONTAINER_RUNTIME picks the runtime (default `docker`; `podman` works too), and
# CONTAINER_RUN_ARGS adds arguments to every `run` (rootless podman needs `--user 0` for
# Stalwart). The container, named calendar-sync-SERVER, is left running for its logs; remove it
# with `docker rm -f calendar-sync-SERVER` and its volumes with `docker volume rm`.
set -eu

RUSTICAL_IMAGE="ghcr.io/lennart-k/rustical:0.19.2@sha256:f9124b07a7dddcba293ed0e31e9bf14ac47a70930bc89fd4f0d3a665205e7b27"
STALWART_IMAGE="ghcr.io/stalwartlabs/stalwart:v1.1.1@sha256:5ebdf474b237161742fe4309396af4ae8797c938e8e6eb7e2d4837332c4f9851"

if [ "$#" -lt 2 ]; then
  echo "usage: $0 rustical|stalwart COMMAND..." >&2
  exit 2
fi
server="$1"
shift
runtime="${CONTAINER_RUNTIME:-docker}"
name="calendar-sync-$server"

# Runs the container runtime with CONTAINER_RUN_ARGS after `run`.
run() {
  # shellcheck disable=SC2086 # CONTAINER_RUN_ARGS is a list of arguments.
  "$runtime" run ${CONTAINER_RUN_ARGS:-} "$@"
}

# Polls URL until it answers STATUS, for at most 60 seconds. POSIX sh has no local variables, so
# these names are the function's own.
await() {
  await_url="$1"
  await_status="$2"
  shift 2
  await_tries=0
  while [ "$(curl -s -o /dev/null -w '%{http_code}' "$@" "$await_url" || true)" != "$await_status" ]; do
    await_tries=$((await_tries + 1))
    if [ "$await_tries" -ge 60 ]; then
      echo "$await_url did not answer $await_status within 60 seconds" >&2
      "$runtime" logs "$name" >&2 || true
      exit 1
    fi
    sleep 1
  done
}

case "$server" in
  rustical)
    port="${RUSTICAL_PORT:-4000}"
    user=ci
    "$runtime" volume create "$name-data" >/dev/null
    run --rm -v "$name-data:/var/lib/rustical" "$RUSTICAL_IMAGE" \
      /usr/local/bin/rustical principals create "$user" --name CI >&2
    # `app-token create` prints the token on its last line; CalDAV clients authenticate with it.
    password="$(run --rm -v "$name-data:/var/lib/rustical" "$RUSTICAL_IMAGE" \
      /usr/local/bin/rustical principals app-token create "$user" --name calendar-sync | tail -n 1)"
    run -d --name "$name" -p "127.0.0.1:$port:4000" -v "$name-data:/var/lib/rustical" \
      "$RUSTICAL_IMAGE" >/dev/null
    url="http://127.0.0.1:$port"
    collection="/caldav/principal/$user/calendar-sync/"
    await "$url/caldav/principal/$user/" 207 -u "$user:$password" -X PROPFIND -H 'Depth: 0'
    # RFC 4791 section 5.3.1: a new calendar collection.
    status="$(curl -s -o /dev/null -w '%{http_code}' -u "$user:$password" -X MKCALENDAR \
      -H 'Content-Type: application/xml; charset=utf-8' \
      --data '<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:set><D:prop><D:displayname>calendar-sync</D:displayname></D:prop></D:set></C:mkcalendar>' \
      "$url$collection")"
    if [ "$status" != 201 ]; then
      echo "MKCALENDAR $collection answered $status" >&2
      exit 1
    fi
    ;;
  stalwart)
    port="${STALWART_PORT:-8080}"
    # The recovery administrator exists only while the server bootstraps, on 127.0.0.1.
    recovery="calendar-sync-recovery"
    "$runtime" volume create "$name-etc" >/dev/null
    "$runtime" volume create "$name-data" >/dev/null
    run -d --name "$name" -p "127.0.0.1:$port:8080" \
      -v "$name-etc:/etc/stalwart" -v "$name-data:/var/lib/stalwart" \
      -e "STALWART_RECOVERY_ADMIN=admin:$recovery" "$STALWART_IMAGE" >/dev/null
    url="http://127.0.0.1:$port"
    await "$url/jmap/session" 200 -u "admin:$recovery"
    # With no configuration the server starts in bootstrap mode, where the JMAP API accepts only
    # the Bootstrap object. Setting it creates the domain and its first administrator, a real
    # account with a calendar, and answers with that account's generated credentials.
    answer="$(curl -s -u "admin:$recovery" -H 'Content-Type: application/json' "$url/jmap/" \
      --data '{"using":["urn:ietf:params:jmap:core"],"methodCalls":[["x:Bootstrap/set",{"update":{"singleton":{"defaultHostname":"localhost","defaultDomain":"example.com","requestTlsCertificate":false,"generateDkimKeys":false}}},"0"]]}')"
    user="$(printf '%s' "$answer" | sed -n 's/.*"username":"\([^"]*\)".*/\1/p')"
    password="$(printf '%s' "$answer" | sed -n 's/.*"secret":"\([^"]*\)".*/\1/p')"
    if [ -z "$user" ] || [ -z "$password" ]; then
      echo "the Bootstrap object was not set: $answer" >&2
      exit 1
    fi
    # Bootstrap mode ends with a restart; port 8080 then serves every HTTP service, CalDAV too.
    "$runtime" restart "$name" >/dev/null
    await "$url/healthz/live" 200
    encoded="$(printf '%s' "$user" | sed 's/@/%40/')"
    collection="/dav/cal/$encoded/default/"
    await "$url$collection" 207 -u "$user:$password" -X PROPFIND -H 'Depth: 0'
    ;;
  *)
    echo "unknown server: $server (rustical or stalwart)" >&2
    exit 2
    ;;
esac

CALENDAR_SYNC_CALDAV_LIVE=1
CALDAV_URL="$url"
CALDAV_COLLECTION="$collection"
CALDAV_USERNAME="$user"
CALDAV_PASSWORD="$password"
export CALENDAR_SYNC_CALDAV_LIVE CALDAV_URL CALDAV_COLLECTION CALDAV_USERNAME CALDAV_PASSWORD
exec "$@"

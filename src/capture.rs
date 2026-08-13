use http::header::{HeaderName, InvalidHeaderName};
use outlet::{RequestData, ResponseData};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
enum HeaderSelection {
    All,
    Allow(HashSet<HeaderName>),
}

impl HeaderSelection {
    fn allow<I, S>(headers: I) -> Result<Self, InvalidHeaderName>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        headers
            .into_iter()
            .map(|name| HeaderName::from_bytes(name.as_ref().as_bytes()))
            .collect::<Result<HashSet<_>, _>>()
            .map(Self::Allow)
    }

    fn apply(&self, headers: &mut HashMap<String, Vec<bytes::Bytes>>) {
        let mut entries = std::mem::take(headers).into_iter().collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));

        for (name, values) in entries {
            let canonical_name = match HeaderName::from_bytes(name.as_bytes()) {
                Ok(canonical_name) => canonical_name,
                Err(_) if matches!(self, Self::All) => {
                    headers.entry(name).or_default().extend(values);
                    continue;
                }
                Err(_) => continue,
            };
            let retained = match self {
                Self::All => true,
                Self::Allow(allowed) => allowed.contains(&canonical_name),
            };
            if retained {
                headers
                    .entry(canonical_name.as_str().to_owned())
                    .or_default()
                    .extend(values);
            }
        }
    }

    fn explicitly_allows(&self, header: &HeaderName) -> bool {
        matches!(self, Self::Allow(allowed) if allowed.contains(header))
    }
}

/// Controls which HTTP headers are made available to persistent capture.
///
/// The default preserves all headers for backwards compatibility. Applications
/// handling sensitive traffic should configure explicit request and response
/// allowlists.
#[derive(Clone, Debug)]
pub struct CapturePolicy {
    request_headers: HeaderSelection,
    response_headers: HeaderSelection,
    subject_header: Option<HeaderName>,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self::all()
    }
}

impl CapturePolicy {
    /// Preserve all request and response headers.
    pub fn all() -> Self {
        Self {
            request_headers: HeaderSelection::All,
            response_headers: HeaderSelection::All,
            subject_header: None,
        }
    }

    /// Apply the same header allowlist to requests and responses.
    pub fn allow_headers<I, S>(headers: I) -> Result<Self, InvalidHeaderName>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let selection = HeaderSelection::allow(headers)?;
        Ok(Self {
            request_headers: selection.clone(),
            response_headers: selection,
            subject_header: None,
        })
    }

    /// Replace the request header allowlist.
    pub fn with_request_headers<I, S>(mut self, headers: I) -> Result<Self, InvalidHeaderName>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.request_headers = HeaderSelection::allow(headers)?;
        Ok(self)
    }

    /// Replace the response header allowlist.
    pub fn with_response_headers<I, S>(mut self, headers: I) -> Result<Self, InvalidHeaderName>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.response_headers = HeaderSelection::allow(headers)?;
        Ok(self)
    }

    /// Extract an opaque subject identifier from a request header before
    /// applying the request allowlist.
    pub fn with_subject_header(
        mut self,
        header: impl AsRef<str>,
    ) -> Result<Self, InvalidHeaderName> {
        self.subject_header = Some(HeaderName::from_bytes(header.as_ref().as_bytes())?);
        Ok(self)
    }

    pub(crate) fn prepare_request(&self, mut data: RequestData) -> PreparedRequest {
        let subject_id = self.subject_id(&data);
        self.remove_unlisted_subject_carrier(&mut data.headers);
        self.request_headers.apply(&mut data.headers);
        PreparedRequest { data, subject_id }
    }

    pub(crate) fn prepare_response(
        &self,
        mut request: RequestData,
        mut response: ResponseData,
    ) -> PreparedResponse {
        let subject_id = self.subject_id(&request);
        self.remove_unlisted_subject_carrier(&mut request.headers);
        self.request_headers.apply(&mut request.headers);
        self.response_headers.apply(&mut response.headers);
        PreparedResponse {
            request,
            response,
            subject_id,
        }
    }

    fn subject_id(&self, request: &RequestData) -> Option<String> {
        let subject_header = self.subject_header.as_ref()?;
        let mut values = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(subject_header.as_str()))
            .flat_map(|(_, values)| values.iter());
        let value = values.next()?;
        if values.next().is_some() || value.is_empty() {
            return None;
        }
        std::str::from_utf8(value).ok().map(str::to_owned)
    }

    fn remove_unlisted_subject_carrier(&self, headers: &mut HashMap<String, Vec<bytes::Bytes>>) {
        let Some(subject_header) = self.subject_header.as_ref() else {
            return;
        };
        if !self.request_headers.explicitly_allows(subject_header) {
            headers.retain(|name, _| !name.eq_ignore_ascii_case(subject_header.as_str()));
        }
    }
}

pub(crate) struct PreparedRequest {
    pub(crate) data: RequestData,
    pub(crate) subject_id: Option<String>,
}

pub(crate) struct PreparedResponse {
    pub(crate) request: RequestData,
    pub(crate) response: ResponseData,
    pub(crate) subject_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use outlet::{RequestData, ResponseData};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn request(headers: impl IntoIterator<Item = (&'static str, Vec<Bytes>)>) -> RequestData {
        RequestData {
            correlation_id: 7,
            timestamp: SystemTime::UNIX_EPOCH,
            method: http::Method::POST,
            uri: http::Uri::from_static("/requests"),
            headers: headers
                .into_iter()
                .map(|(name, values)| (name.to_owned(), values))
                .collect::<HashMap<_, _>>(),
            body: Some(Bytes::from_static(br#"{"prompt":"hello"}"#)),
            trace_id: Some("trace-id".to_owned()),
            span_id: Some("span-id".to_owned()),
        }
    }

    fn response(headers: impl IntoIterator<Item = (&'static str, Vec<Bytes>)>) -> ResponseData {
        ResponseData {
            correlation_id: 7,
            timestamp: SystemTime::UNIX_EPOCH,
            status: http::StatusCode::OK,
            headers: headers
                .into_iter()
                .map(|(name, values)| (name.to_owned(), values))
                .collect::<HashMap<_, _>>(),
            body: Some(Bytes::from_static(br#"{"result":"hello"}"#)),
            duration_to_first_byte: Duration::from_millis(10),
            duration: Duration::from_millis(20),
            extensions: Default::default(),
        }
    }

    #[test]
    fn allowlist_is_case_insensitive_and_canonicalises_persisted_names() {
        let policy = CapturePolicy::allow_headers(["Content-Type", "X-Request-ID"])
            .expect("valid header names");
        let prepared = policy.prepare_request(request([
            (
                "CONTENT-TYPE",
                vec![Bytes::from_static(b"application/json")],
            ),
            ("x-request-id", vec![Bytes::from_static(b"req-1")]),
            ("authorization", vec![Bytes::from_static(b"Bearer secret")]),
        ]));

        assert_eq!(prepared.data.headers.len(), 2);
        assert_eq!(
            prepared.data.headers["content-type"][0],
            Bytes::from_static(b"application/json")
        );
        assert_eq!(
            prepared.data.headers["x-request-id"][0],
            Bytes::from_static(b"req-1")
        );
        assert!(!prepared.data.headers.contains_key("authorization"));
    }

    #[test]
    fn default_preserves_all_values_with_canonical_names_and_no_subject() {
        let prepared = CapturePolicy::all().prepare_request(request([
            ("x-request-id", vec![Bytes::from_static(b"lower")]),
            ("X-Request-ID", vec![Bytes::from_static(b"upper")]),
            (
                "Content-Type",
                vec![Bytes::from_static(b"application/json")],
            ),
        ]));

        assert_eq!(prepared.subject_id, None);
        assert_eq!(prepared.data.headers.len(), 2);
        assert_eq!(
            prepared.data.headers["x-request-id"],
            vec![Bytes::from_static(b"upper"), Bytes::from_static(b"lower")]
        );
        assert_eq!(
            prepared.data.headers["content-type"],
            vec![Bytes::from_static(b"application/json")]
        );
    }

    #[test]
    fn subject_is_extracted_before_its_carrier_is_removed() {
        let policy = CapturePolicy::allow_headers(["content-type"])
            .unwrap()
            .with_subject_header("x-capture-subject")
            .unwrap();
        let prepared = policy.prepare_request(request([
            (
                "x-capture-subject",
                vec![Bytes::from_static(b"18c27488-7cf8-4cca-a90b-2cb472a9979f")],
            ),
            (
                "content-type",
                vec![Bytes::from_static(b"application/json")],
            ),
        ]));

        assert_eq!(
            prepared.subject_id.as_deref(),
            Some("18c27488-7cf8-4cca-a90b-2cb472a9979f")
        );
        assert!(!prepared.data.headers.contains_key("x-capture-subject"));
    }

    #[test]
    fn subject_carrier_requires_explicit_allowlisting() {
        let policy = CapturePolicy::all()
            .with_subject_header("x-capture-subject")
            .unwrap();
        let prepared = policy.prepare_request(request([(
            "x-capture-subject",
            vec![Bytes::from_static(b"subject-1")],
        )]));

        assert_eq!(prepared.subject_id.as_deref(), Some("subject-1"));
        assert!(!prepared.data.headers.contains_key("x-capture-subject"));

        let explicitly_allowed = CapturePolicy::allow_headers(["x-capture-subject"])
            .unwrap()
            .with_subject_header("x-capture-subject")
            .unwrap()
            .prepare_request(request([(
                "x-capture-subject",
                vec![Bytes::from_static(b"subject-1")],
            )]));
        assert!(explicitly_allowed
            .data
            .headers
            .contains_key("x-capture-subject"));
    }

    #[test]
    fn invalid_or_empty_subject_is_not_persisted() {
        let policy = CapturePolicy::all()
            .with_subject_header("x-capture-subject")
            .unwrap();

        for value in [Bytes::new(), Bytes::from_static(b"\xff")] {
            let prepared = policy.prepare_request(request([("x-capture-subject", vec![value])]));
            assert_eq!(prepared.subject_id, None);
        }
    }

    #[test]
    fn ambiguous_multi_value_subject_is_not_persisted() {
        let policy = CapturePolicy::all()
            .with_subject_header("x-capture-subject")
            .unwrap();
        let prepared = policy.prepare_request(request([(
            "x-capture-subject",
            vec![
                Bytes::from_static(b"subject-1"),
                Bytes::from_static(b"subject-2"),
            ],
        )]));

        assert_eq!(prepared.subject_id, None);
    }

    #[test]
    fn canonical_name_collisions_preserve_all_values_deterministically() {
        let policy = CapturePolicy::allow_headers(["x-request-id"]).unwrap();
        let prepared = policy.prepare_request(request([
            ("x-request-id", vec![Bytes::from_static(b"lower")]),
            ("X-Request-ID", vec![Bytes::from_static(b"upper")]),
        ]));

        assert_eq!(
            prepared.data.headers["x-request-id"],
            vec![Bytes::from_static(b"upper"), Bytes::from_static(b"lower")]
        );
    }

    #[test]
    fn request_and_response_allowlists_are_independent() {
        let policy = CapturePolicy::all()
            .with_request_headers(["content-type"])
            .unwrap()
            .with_response_headers(["cache-control"])
            .unwrap();
        let prepared = policy.prepare_response(
            request([
                (
                    "content-type",
                    vec![Bytes::from_static(b"application/json")],
                ),
                ("authorization", vec![Bytes::from_static(b"secret")]),
            ]),
            response([
                ("cache-control", vec![Bytes::from_static(b"no-store")]),
                ("set-cookie", vec![Bytes::from_static(b"secret")]),
            ]),
        );

        assert_eq!(prepared.request.headers.len(), 1);
        assert_eq!(prepared.response.headers.len(), 1);
        assert!(prepared.request.headers.contains_key("content-type"));
        assert!(prepared.response.headers.contains_key("cache-control"));
    }
}

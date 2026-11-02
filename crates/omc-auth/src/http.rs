//! HTTP client adapter that adapts oauth2 token requests to a provider.

use std::future::Future;
use std::pin::Pin;

use oauth2::http::{HeaderName, HeaderValue, header::CONTENT_TYPE};
use oauth2::{AsyncHttpClient, HttpClientError, HttpRequest, HttpResponse};

/// Wraps a reqwest client, optionally re-encoding bodies as JSON and adding headers.
pub struct ProviderHttp {
    /// The underlying client; redirects are disabled.
    inner: oauth2::reqwest::Client,
    /// Whether to rewrite form bodies as JSON.
    json_body: bool,
    /// Headers added to every request.
    headers: &'static [(&'static str, &'static str)],
}

impl ProviderHttp {
    /// Creates an adapter with the given body encoding and extra headers.
    ///
    /// # Errors
    ///
    /// Returns the reqwest error when the client cannot be built.
    pub fn new(
        json_body: bool,
        headers: &'static [(&'static str, &'static str)],
    ) -> Result<Self, oauth2::reqwest::Error> {
        let inner = oauth2::reqwest::Client::builder()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            inner,
            json_body,
            headers,
        })
    }

    /// Applies the JSON re-encoding and extra headers to `request`.
    fn rewrite(&self, mut request: HttpRequest) -> HttpRequest {
        if self.json_body {
            let fields: std::collections::BTreeMap<String, String> =
                oauth2::url::form_urlencoded::parse(request.body())
                    .into_owned()
                    .collect();
            *request.body_mut() = serde_json::to_vec(&fields).unwrap_or_default();
            request
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        for (name, value) in self.headers {
            request.headers_mut().insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        request
    }
}

impl<'c> AsyncHttpClient<'c> for ProviderHttp {
    type Error = HttpClientError<oauth2::reqwest::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + Sync + 'c>>;

    /// Sends the rewritten request through the inner reqwest client.
    fn call(&'c self, request: HttpRequest) -> Self::Future {
        self.inner.call(self.rewrite(request))
    }
}

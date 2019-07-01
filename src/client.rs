//! The HTTP client implementation.

use crate::config::*;
use crate::handler;
use crate::handler::RequestHandler;
use crate::middleware::Middleware;
use crate::{agent, Body, Error};
use futures::executor::block_on;
use futures::prelude::*;
use http::{Request, Response};
use lazy_static::lazy_static;
use std::fmt;
use std::iter::FromIterator;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::*;
use std::time::Duration;

lazy_static! {
    static ref USER_AGENT: String = format!(
        "curl/{} chttp/{}",
        curl::Version::get().version(),
        env!("CARGO_PKG_VERSION")
    );
}

/// An HTTP client builder, capable of creating custom [`Client`] instances with
/// customized behavior.
///
/// Example:
///
/// ```rust
/// use chttp::config::RedirectPolicy;
/// use chttp::http;
/// use chttp::prelude::*;
/// use std::time::Duration;
///
/// # fn run() -> Result<(), chttp::Error> {
/// let client = Client::builder()
///     .timeout(Duration::from_secs(60))
///     .redirect_policy(RedirectPolicy::Limit(10))
///     .preferred_http_version(http::Version::HTTP_2)
///     .build()?;
///
/// let mut response = client.get("https://example.org")?;
/// let body = response.body_mut().text()?;
/// println!("{}", body);
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct ClientBuilder {
    defaults: http::Extensions,
    middleware: Vec<Box<dyn Middleware>>,
}

impl ClientBuilder {
    /// Create a new builder for building a custom client.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable persistent cookie handling using a cookie jar.
    #[cfg(feature = "cookies")]
    pub fn cookies(self) -> Self {
        self.middleware_impl(crate::cookies::CookieJar::default())
    }

    /// Add a middleware layer to the client.
    #[cfg(feature = "middleware-api")]
    pub fn middleware(self, middleware: impl Middleware) -> Self {
        self.middleware_impl(middleware)
    }

    #[allow(unused)]
    fn middleware_impl(mut self, middleware: impl Middleware) -> Self {
        self.middleware.push(Box::new(middleware));
        self
    }

    /// Set a timeout for the maximum time allowed for a request-response cycle.
    ///
    /// If not set, no timeout will be enforced.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.defaults.insert(Timeout(timeout));
        self
    }

    /// Set a timeout for the initial connection phase.
    ///
    /// If not set, a connect timeout of 300 seconds will be used.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.defaults.insert(ConnectTimeout(timeout));
        self
    }

    /// Set a policy for automatically following server redirects.
    ///
    /// The default is to not follow redirects.
    pub fn redirect_policy(mut self, policy: RedirectPolicy) -> Self {
        self.defaults.insert(policy);
        self
    }

    /// Update the `Referer` header automatically when following redirects.
    pub fn auto_referer(mut self) -> Self {
        self.defaults.insert(AutoReferer);
        self
    }

    /// Set a preferred HTTP version the client should attempt to use to
    /// communicate to the server with.
    ///
    /// This is treated as a suggestion. A different version may be used if the
    /// server does not support it or negotiates a different version.
    pub fn preferred_http_version(mut self, version: http::Version) -> Self {
        self.defaults.insert(PreferredHttpVersion(version));
        self
    }

    /// Enable TCP keepalive with a given probe interval.
    pub fn tcp_keepalive(mut self, interval: Duration) -> Self {
        self.defaults.insert(TcpKeepAlive(interval));
        self
    }

    /// Enables the `TCP_NODELAY` option on connect.
    pub fn tcp_nodelay(mut self) -> Self {
        self.defaults.insert(TcpNoDelay);
        self
    }

    /// Set a proxy to use for requests.
    ///
    /// The proxy protocol is specified by the URI scheme.
    ///
    /// - **`http`**: Proxy. Default when no scheme is specified.
    /// - **`https`**: HTTPS Proxy. (Added in 7.52.0 for OpenSSL, GnuTLS and
    ///   NSS)
    /// - **`socks4`**: SOCKS4 Proxy.
    /// - **`socks4a`**: SOCKS4a Proxy. Proxy resolves URL hostname.
    /// - **`socks5`**: SOCKS5 Proxy.
    /// - **`socks5h`**: SOCKS5 Proxy. Proxy resolves URL hostname.
    pub fn proxy(mut self, proxy: http::Uri) -> Self {
        self.defaults.insert(Proxy(proxy));
        self
    }

    /// Set a maximum upload speed for the request body, in bytes per second.
    ///
    /// The default is unlimited.
    pub fn max_upload_speed(mut self, max: u64) -> Self {
        self.defaults.insert(MaxUploadSpeed(max));
        self
    }

    /// Set a maximum download speed for the response body, in bytes per second.
    ///
    /// The default is unlimited.
    pub fn max_download_speed(mut self, max: u64) -> Self {
        self.defaults.insert(MaxDownloadSpeed(max));
        self
    }

    /// Set a list of specific DNS servers to be used for DNS resolution.
    ///
    /// By default this option is not set and the system's built-in DNS resolver
    /// is used. This option can only be used if libcurl is compiled with
    /// [c-ares](https://c-ares.haxx.se), otherwise this option has no effect.
    pub fn dns_servers(mut self, servers: impl IntoIterator<Item = SocketAddr>) -> Self {
        self.defaults.insert(DnsServers::from_iter(servers));
        self
    }

    /// Set a list of ciphers to use for SSL/TLS connections.
    ///
    /// The list of valid cipher names is dependent on the underlying SSL/TLS
    /// engine in use.
    ///
    /// You can find an up-to-date list of potential cipher names at
    /// <https://curl.haxx.se/docs/ssl-ciphers.html>.
    ///
    /// The default is unset and will result in the system defaults being used.
    pub fn ssl_ciphers(mut self, servers: impl IntoIterator<Item = String>) -> Self {
        self.defaults.insert(SslCiphers::from_iter(servers));
        self
    }

    /// Set a custom SSL/TLS client certificate to use for all client
    /// connections.
    ///
    /// If a format is not supported by the underlying SSL/TLS engine, an error
    /// will be returned when attempting to send a request using the offending
    /// certificate.
    ///
    /// The default value is none.
    ///
    /// # Examples
    ///
    /// ```
    /// # use chttp::config::*;
    /// # use chttp::prelude::*;
    /// #
    /// # fn run() -> Result<(), chttp::Error> {
    /// let client = Client::builder()
    ///     .ssl_client_certificate(ClientCertificate::PEM {
    ///         path: "client.pem".into(),
    ///         private_key: Some(PrivateKey::PEM {
    ///             path: "key.pem".into(),
    ///             password: Some("secret".into()),
    ///         }),
    ///     })
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn ssl_client_certificate(mut self, certificate: ClientCertificate) -> Self {
        self.defaults.insert(certificate);
        self
    }

    /// Build an HTTP client using the configured options.
    ///
    /// If the client fails to initialize, an error will be returned.
    pub fn build(self) -> Result<Client, Error> {
        Ok(Client {
            agent: agent::new()?,
            defaults: self.defaults,
            middleware: self.middleware,
        })
    }
}

/// An HTTP client for making requests.
///
/// The client maintains a connection pool internally and is expensive to
/// create, so we recommend re-using your clients instead of discarding and
/// recreating them.
pub struct Client {
    agent: agent::Handle,
    defaults: http::Extensions,
    middleware: Vec<Box<dyn Middleware>>,
}

impl Client {
    /// Create a new HTTP client using the default configuration.
    ///
    /// Panics if any internal systems failed to initialize during creation.
    /// This might occur if creating a socket fails, spawning a thread fails, or
    /// if something else goes wrong.
    pub fn new() -> Self {
        ClientBuilder::default()
            .build()
            .expect("client failed to initialize")
    }

    /// Get a reference to a global client instance.
    pub(crate) fn shared() -> &'static Self {
        lazy_static! {
            static ref CLIENT: Client = Client::new();
        }
        &CLIENT
    }

    /// Create a new builder for building a custom client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Sends an HTTP GET request.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn get<U>(&self, uri: U) -> Result<Response<Body>, Error>
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        block_on(self.get_async(uri))
    }

    /// Sends an HTTP GET request asynchronously.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    #[auto_enums::auto_enum(Future)]
    pub fn get_async<U>(&self, uri: U) -> impl Future<Output = Result<Response<Body>, Error>> + '_
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        match http::Request::get(uri).body(Body::empty()) {
            Ok(request) => self.send_async(request),
            Err(e) => future::err(e.into()),
        }
    }

    /// Sends an HTTP HEAD request.
    pub fn head<U>(&self, uri: U) -> Result<Response<Body>, Error>
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        block_on(self.head_async(uri))
    }

    /// Sends an HTTP HEAD request asynchronously.
    #[auto_enums::auto_enum(Future)]
    pub fn head_async<U>(&self, uri: U) -> impl Future<Output = Result<Response<Body>, Error>> + '_
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        match http::Request::head(uri).body(Body::empty()) {
            Ok(request) => self.send_async(request),
            Err(e) => future::err(e.into()),
        }
    }

    /// Sends an HTTP POST request.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn post<U>(&self, uri: U, body: impl Into<Body>) -> Result<Response<Body>, Error>
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        block_on(self.post_async(uri, body))
    }

    /// Sends an HTTP POST request asynchronously.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    #[auto_enums::auto_enum(Future)]
    pub fn post_async<U>(
        &self,
        uri: U,
        body: impl Into<Body>,
    ) -> impl Future<Output = Result<Response<Body>, Error>> + '_
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        match http::Request::post(uri).body(body) {
            Ok(request) => self.send_async(request),
            Err(e) => future::err(e.into()),
        }
    }

    /// Sends an HTTP PUT request.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn put<U>(&self, uri: U, body: impl Into<Body>) -> Result<Response<Body>, Error>
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        block_on(self.put_async(uri, body))
    }

    /// Sends an HTTP PUT request asynchronously.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    #[auto_enums::auto_enum(Future)]
    pub fn put_async<U>(
        &self,
        uri: U,
        body: impl Into<Body>,
    ) -> impl Future<Output = Result<Response<Body>, Error>> + '_
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        match http::Request::put(uri).body(body) {
            Ok(request) => self.send_async(request),
            Err(e) => future::err(e.into()),
        }
    }

    /// Sends an HTTP DELETE request.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn delete<U>(&self, uri: U) -> Result<Response<Body>, Error>
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        block_on(self.delete_async(uri))
    }

    /// Sends an HTTP DELETE request asynchronously.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    #[auto_enums::auto_enum(Future)]
    pub fn delete_async<U>(&self, uri: U) -> impl Future<Output = Result<Response<Body>, Error>> + '_
    where
        http::Uri: http::HttpTryFrom<U>,
    {
        match http::Request::delete(uri).body(Body::empty()) {
            Ok(request) => self.send_async(request),
            Err(e) => future::err(e.into()),
        }
    }

    /// Sends a request and returns the response.
    ///
    /// The request may include [extensions](http::Extensions) to customize how
    /// it is sent. If the request contains an [`Options`] struct as an
    /// extension, then those options will be used instead of the default
    /// options this client is configured with.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn send<B: Into<Body>>(&self, request: Request<B>) -> Result<Response<Body>, Error> {
        block_on(self.send_async(request))
    }

    /// Begin sending a request and return a future of the response.
    ///
    /// The request may include [extensions](http::Extensions) to customize how
    /// it is sent. If the request contains an [`Options`] struct as an
    /// extension, then those options will be used instead of the default
    /// options this client is configured with.
    ///
    /// The response body is provided as a stream that may only be consumed
    /// once.
    pub fn send_async<B: Into<Body>>(&self, request: Request<B>) -> ResponseFuture {
        let mut request = request.map(Into::into);

        // Set default user agent if not specified.
        request
            .headers_mut()
            .entry(http::header::USER_AGENT)
            .unwrap()
            .or_insert(USER_AGENT.parse().unwrap());

        // Apply any request middleware, starting with the outermost one.
        for middleware in self.middleware.iter().rev() {
            request = middleware.filter_request(request);
        }

        ResponseFuture {
            client: self,
            request: Some(request),
            inner: None,
        }
    }

    fn create_easy_handle(
        &self,
        request: Request<Body>,
    ) -> Result<(curl::easy::Easy2<RequestHandler>, handler::ResponseFuture), Error> {
        // Prepare the request plumbing.
        let (parts, body) = request.into_parts();
        let body_is_empty = body.is_empty();
        let body_size = body.len();
        let (handler, future) = RequestHandler::new(body);

        // Helper for fetching an extension first from the request, then falling
        // back to client defaults.
        macro_rules! extension {
            ($first:expr) => {
                $first.get()
            };

            ($first:expr, $($rest:expr),+) => {
                $first.get().or_else(|| extension!($($rest),*))
            };
        }

        let mut easy = curl::easy::Easy2::new(handler);

        easy.verbose(log::log_enabled!(log::Level::Trace))?;
        easy.signal(false)?;

        if let Some(Timeout(timeout)) = extension!(parts.extensions, self.defaults) {
            easy.timeout(*timeout)?;
        }

        if let Some(ConnectTimeout(timeout)) = extension!(parts.extensions, self.defaults) {
            easy.connect_timeout(*timeout)?;
        }

        if let Some(TcpKeepAlive(interval)) = extension!(parts.extensions, self.defaults) {
            easy.tcp_keepalive(true)?;
            easy.tcp_keepintvl(*interval)?;
        }

        if let Some(TcpNoDelay) = extension!(parts.extensions, self.defaults) {
            easy.tcp_nodelay(true)?;
        }

        if let Some(redirect_policy) = extension!(parts.extensions, self.defaults) {
            match redirect_policy {
                RedirectPolicy::Follow => {
                    easy.follow_location(true)?;
                }
                RedirectPolicy::Limit(max) => {
                    easy.follow_location(true)?;
                    easy.max_redirections(*max)?;
                }
                RedirectPolicy::None => {
                    easy.follow_location(false)?;
                }
            }
        }

        if let Some(MaxUploadSpeed(limit)) = extension!(parts.extensions, self.defaults) {
            easy.max_send_speed(*limit)?;
        }

        if let Some(MaxDownloadSpeed(limit)) = extension!(parts.extensions, self.defaults) {
            easy.max_recv_speed(*limit)?;
        }

        // Set a preferred HTTP version to negotiate.
        easy.http_version(match extension!(parts.extensions, self.defaults) {
            Some(PreferredHttpVersion(http::Version::HTTP_10)) => curl::easy::HttpVersion::V10,
            Some(PreferredHttpVersion(http::Version::HTTP_11)) => curl::easy::HttpVersion::V11,
            Some(PreferredHttpVersion(http::Version::HTTP_2)) => curl::easy::HttpVersion::V2,
            _ => curl::easy::HttpVersion::Any,
        })?;

        if let Some(Proxy(proxy)) = extension!(parts.extensions, self.defaults) {
            easy.proxy(&format!("{}", proxy))?;
        }

        if let Some(DnsServers(addrs)) = extension!(parts.extensions, self.defaults) {
            let dns_string = addrs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            if let Err(e) = easy.dns_servers(&dns_string) {
                log::warn!("DNS servers could not be configured: {}", e);
            }
        }

        // Configure SSL options.
        if let Some(SslCiphers(ciphers)) = extension!(parts.extensions, self.defaults) {
            easy.ssl_cipher_list(&ciphers.join(":"))?;
        }

        if let Some(cert) = extension!(parts.extensions, self.defaults) {
            easy.ssl_client_certificate(cert)?;
        }

        // Enable automatic response decompression.
        easy.accept_encoding("")?;

        // Set the request data according to the request given.
        easy.custom_request(parts.method.as_str())?;
        easy.url(&parts.uri.to_string())?;

        let mut headers = curl::easy::List::new();
        for (name, value) in parts.headers.iter() {
            let header = format!("{}: {}", name.as_str(), value.to_str().unwrap());
            headers.append(&header)?;
        }
        easy.http_headers(headers)?;

        // If the request body is non-empty, tell curl that we are going to
        // upload something.
        if !body_is_empty {
            easy.upload(true)?;

            if let Some(len) = body_size {
                // If we know the size of the request body up front, tell curl
                // about it.
                easy.in_filesize(len as u64)?;
            }
        }

        Ok((easy, future))
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Client").finish()
    }
}

/// Helper extension methods for curl easy handles.
trait EasyExt {
    fn easy(&mut self) -> &mut curl::easy::Easy2<RequestHandler>;

    fn ssl_client_certificate(&mut self, cert: &ClientCertificate) -> Result<(), curl::Error> {
        match cert {
            ClientCertificate::PEM { path, private_key } => {
                self.easy().ssl_cert(path)?;
                self.easy().ssl_cert_type("PEM")?;
                if let Some(key) = private_key {
                    self.ssl_private_key(key)?;
                }
            }
            ClientCertificate::DER { path, private_key } => {
                self.easy().ssl_cert(path)?;
                self.easy().ssl_cert_type("DER")?;
                if let Some(key) = private_key {
                    self.ssl_private_key(key)?;
                }
            }
            ClientCertificate::P12 { path, password } => {
                self.easy().ssl_cert(path)?;
                self.easy().ssl_cert_type("P12")?;
                if let Some(password) = password {
                    self.easy().key_password(password)?;
                }
            }
        }

        Ok(())
    }

    fn ssl_private_key(&mut self, key: &PrivateKey) -> Result<(), curl::Error> {
        match key {
            PrivateKey::PEM { path, password } => {
                self.easy().ssl_key(path)?;
                self.easy().ssl_key_type("PEM")?;
                if let Some(password) = password {
                    self.easy().key_password(password)?;
                }
            }
            PrivateKey::DER { path, password } => {
                self.easy().ssl_key(path)?;
                self.easy().ssl_key_type("DER")?;
                if let Some(password) = password {
                    self.easy().key_password(password)?;
                }
            }
        }

        Ok(())
    }
}

impl EasyExt for curl::easy::Easy2<RequestHandler> {
    fn easy(&mut self) -> &mut Self {
        self
    }
}

pub struct ResponseFuture<'c> {
    client: &'c Client,
    request: Option<Request<Body>>,
    inner: Option<handler::ResponseFuture>,
}

impl Future for ResponseFuture<'_> {
    type Output = Result<Response<Body>, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Request has not been sent yet.
        if let Some(request) = self.request.take() {
            // Create and configure a curl easy handle to fulfil the request.
            let (easy, future) = self.client.create_easy_handle(request)?;

            // Send the request to the agent to be executed.
            self.client.agent.submit_request(easy)?;

            self.inner = Some(future);
        }

        if let Some(inner) = self.inner.as_mut() {
            match inner.poll_unpin(cx) {
                // Buffer isn't full yet.
                Poll::Pending => Poll::Pending,

                // Read error
                Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),

                // Buffer has been filled, try to parse as UTF-8
                Poll::Ready(Ok(mut response)) => {
                    // Apply response middleware, starting with the innermost
                    // one.
                    for middleware in self.client.middleware.iter() {
                        response = middleware.filter_response(response);
                    }

                    Poll::Ready(Ok(response))
                }
            }
        } else {
            Poll::Pending
        }
    }
}

static_assertions::assert_impl!(client; Client, Send, Sync);
static_assertions::assert_impl!(builder; ClientBuilder, Send);

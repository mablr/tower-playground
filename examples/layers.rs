use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};
use tower::{Layer, Service, ServiceBuilder};
use tracing::{Level, error, info};
use tracing_subscriber;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Authentication layer
#[derive(Clone)]
struct AuthLayer;

impl<S> Layer<S> for AuthLayer {
    type Service = AuthMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        AuthMiddleware { inner: service }
    }
}

#[derive(Clone)]
struct AuthMiddleware<S> {
    inner: S,
}

impl<S> Service<String> for AuthMiddleware<S>
where
    S: Service<String> + Clone + Send + 'static,
    S::Response: Send + 'static,
    S::Error: std::fmt::Debug + Send + 'static + From<String>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: String) -> Self::Future {
        let mut inner = self.inner.clone();

        Box::pin(async move {
            if req.starts_with("auth:") {
                let stripped = req.trim_start_matches("auth:").to_string();
                info!("[Auth] Authenticated request: {:?}", stripped);
                inner.call(stripped).await
            } else {
                error!("[Auth] Unauthorized request: {:?}", req);
                Err(Self::Error::from(String::from("Unauthorized")))
            }
        })
    }
}

/// Timing layer
#[derive(Clone)]
struct TimingLayer;

impl<S> Layer<S> for TimingLayer {
    type Service = TimingMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        TimingMiddleware { inner: service }
    }
}

#[derive(Clone)]
struct TimingMiddleware<S> {
    inner: S,
}

impl<S> Service<String> for TimingMiddleware<S>
where
    S: Service<String> + Clone + Send + 'static,
    S::Response: Send + 'static,
    S::Error: std::fmt::Debug + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: String) -> Self::Future {
        info!("[Timing] Incoming request: {:?}", req);
        let start = Instant::now();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let result = inner.call(req).await;
            let elapsed = start.elapsed();
            info!("[Timing] Request handled in {:?}", elapsed);
            result
        })
    }
}

/// Echo service
#[derive(Clone)]
struct EchoService;

impl Service<String> for EchoService {
    type Response = String;
    type Error = String;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: String) -> Self::Future {
        Box::pin(async move { Ok(format!("Echo: {}", req)) })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let mut svc = ServiceBuilder::new()
        .layer(TimingLayer)
        .layer(AuthLayer)
        .service(EchoService);

    let response1 = svc.call("Hello Tower".into());
    println!("Response: {:?}", response1.await);

    let response2 = svc.call("auth:Hello Tower".into());
    let response3 = svc.call("auth:Bye Tower".into());
    let response4 = svc.call("auth:Bye Bye Tower".into());
    println!("Response: {}", response2.await.unwrap());
    println!("Response: {}", response3.await.unwrap());
    println!("Response: {}", response4.await.unwrap());
}

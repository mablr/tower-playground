use std::{collections::HashMap, pin::Pin, sync::Arc, task::{Context, Poll}};
use tokio::sync::Mutex;

use thiserror::Error;
use tower::{Layer, Service, ServiceBuilder};
use tracing::{error, info, Level};


type BoxFuture<T> = Pin<Box<dyn std::future::Future<Output = T> + Send>>;

#[derive(Debug, Error)]
pub enum MediationError {
    #[error("Resource {resource:?} does not exist")]
    ResourceNotFound { resource: Resource },
    #[error("Resource is already reserved")]
    AlreadyReserved,
    #[error("Unhandled request type: {req:?}")]
    UnhandledRequestType { req: Request },
}

#[derive(Debug, Clone)]
pub enum Request {
    RegisterResource { resource: Resource },
    ReserveResources { source: Resource, destination: Resource },
    ReleaseResources { source: Resource, destination: Resource },
    Flow { source: Resource, destination: Resource },
}

#[derive(Debug)]
pub enum Response {
    ResourceRegistered,
    ResourcesReserved { source: Resource, destination: Resource },
    ResourcesReleased { source: Resource, destination: Resource },
    ProvenanceUpdated { destination: Resource },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum Resource {
    File(String),
    Process(u32),
}

#[derive(Default, Clone)]
enum ReservationState {
    #[default]
    Available,
    Read(u64),
    Write,
}

impl ReservationState {
    fn read(&mut self) -> Result<(), MediationError> {
        match self {
            ReservationState::Available => { *self = ReservationState::Read(1); Ok(()) },
            ReservationState::Read(n) => { *self = ReservationState::Read(*n + 1); Ok(()) },
            ReservationState::Write => Err(MediationError::AlreadyReserved),
        }
    }

    fn write(&mut self) -> Result<(), MediationError> {
        match self {
            ReservationState::Available => { *self = ReservationState::Write; Ok(()) },
            ReservationState::Read(_) => Err(MediationError::AlreadyReserved),
            ReservationState::Write => Err(MediationError::AlreadyReserved),
        }
    }

    fn release(&mut self) {
        match self {
            ReservationState::Available => *self = ReservationState::Available,
            ReservationState::Read(n) => *self = if *n > 1 { ReservationState::Read(*n - 1) } else { ReservationState::Available },
            ReservationState::Write => *self = ReservationState::Available,
        }
    }
}



struct TracingLayer;

impl<S> Layer<S> for TracingLayer {
    type Service = TracingMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        TracingMiddleware { inner: service }
    }
}

#[derive(Clone)]
struct TracingMiddleware<S> {
    inner: S,
}

impl<S> Service<Request> for TracingMiddleware<S>
where
    S: Service<Request> + Clone + Send + 'static,
    S::Response: Send + std::fmt::Debug + 'static,
    S::Error: std::fmt::Debug + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        info!("Request: {:?}", req);
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            match inner.call(req).await {
                Ok(response) => {
                    info!("Response: {:?}", response);
                    Ok(response)
                }
                Err(e) => {
                    error!("Error: {:?}", e);
                    Err(e)
                }
            }
        })
    }
}

#[derive(Default, Debug, Clone)]
struct ProvenanceService {
    state: Arc<Mutex<HashMap<Resource, Vec<Resource>>>>,
}

impl ProvenanceService {
    async fn update_prov(&self, source: Resource, destination: Resource) {
        let mut state = self.state.lock().await;

        let mut source_provenance = vec![source.clone()];
        if let Some(mut provenance) = state.get(&source.clone()).cloned() {
            source_provenance.append(&mut provenance);
        }
        
        match state.get_mut(&destination.clone()) {
            Some(destination_provenance) => {
                destination_provenance.append(&mut source_provenance);
                destination_provenance.sort();
                destination_provenance.dedup();
            }
            None => {
                state.insert(destination, source_provenance);
            }
        }
    }
}

impl Service<Request> for ProvenanceService {
    type Response = Response;
    type Error = MediationError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            match req {
                Request::Flow { source, destination } => {
                    this.update_prov(source, destination.clone()).await;
                    Ok(Response::ProvenanceUpdated { destination })
                }
                _ => Err(MediationError::UnhandledRequestType { req }),
            }
        })
    }
}


#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    let provenance = ProvenanceService::default();
    let mut svc = ServiceBuilder::new()
        .layer(TracingLayer)
        .service(provenance.clone());

    let _ = svc.call(Request::Flow { source: Resource::File("file1".to_string()), destination: Resource::File("file2".to_string()) }).await;
    let _ = svc.call(Request::Flow { source: Resource::File("file2".to_string()), destination: Resource::File("file3".to_string()) }).await;

    info!("Provenance: {:?}", provenance.state.lock().await);
}

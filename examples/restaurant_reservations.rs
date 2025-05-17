use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use thiserror::Error;
use tokio::sync::oneshot;
use tower::{Layer, Service, ServiceBuilder};
use tracing::{Level, info, warn};
use tracing_subscriber;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone)]
pub enum Request {
    Reserve { table_id: u32, client_id: u32 },
    Release { reservation_id: u32 },
}

#[derive(Debug)]
pub enum Response {
    Reserved { reservation_id: u32 },
    Released,
}

#[derive(Debug, Error)]
pub enum RestaurantError {
    #[error("Reservation ID {reservation_id} does not exist")]
    ReservationNotFound { reservation_id: u32 },

    #[error("Channel closed unexpectedly")]
    ChannelClosed,

    #[error("Invalid request: {reason}")]
    InvalidRequest { reason: String },

    #[error("Table {id} is already reserved")]
    TableAlreadyReserved { id: u32 },
}

#[derive(Clone)]
struct WaitingState {
    waiting_queues: Arc<Mutex<HashMap<u32, VecDeque<oneshot::Sender<()>>>>>, // table_id -> waiting requests
}

impl WaitingState {
    fn new() -> Self {
        Self {
            waiting_queues: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn add_to_waiting_queue(&self, table_id: u32) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        let mut waiting_queues = self.waiting_queues.lock().unwrap();
        waiting_queues
            .entry(table_id)
            .or_insert_with(VecDeque::new)
            .push_back(tx);
        rx
    }

    fn process_waiting_requests(&self, table_id: u32) {
        let mut waiting_queues = self.waiting_queues.lock().unwrap();
        if let Some(queue) = waiting_queues.get_mut(&table_id) {
            if let Some(request) = queue.pop_front() {
                info!("Table {} released for the next waiting client", table_id);
                let _ = request.send(());
            }
            if queue.is_empty() {
                waiting_queues.remove(&table_id);
            }
        }
    }
}

#[derive(Clone)]
struct ReservationLayer {
    state: WaitingState,
}

impl ReservationLayer {
    fn new() -> Self {
        Self {
            state: WaitingState::new(),
        }
    }
}

impl<S> Layer<S> for ReservationLayer {
    type Service = ReservationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ReservationService {
            inner: service,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
struct ReservationService<S> {
    inner: S,
    state: WaitingState,
}

impl<S> Service<Request> for ReservationService<S>
where
    S: Service<Request, Response = Response, Error = RestaurantError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = RestaurantError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let mut inner = self.inner.clone();
        let state = self.state.clone();

        Box::pin(async move {
            match req {
                Request::Reserve {
                    table_id,
                    client_id,
                } => {
                    // First try to reserve through the inner service
                    let response = inner.call(req.clone()).await;

                    // If the table is already reserved, add to waiting queue
                    if let Err(RestaurantError::TableAlreadyReserved { .. }) = response {
                        let rx = state.add_to_waiting_queue(table_id);
                        info!("Client {} waiting for table {}", client_id, table_id);

                        // Wait for the table to be available
                        rx.await.map_err(|_| RestaurantError::ChannelClosed)?;

                        // Try reservation again
                        inner.call(req).await
                    } else {
                        response
                    }
                }
                Request::Release { reservation_id } => {
                    let response = inner.call(req.clone()).await;

                    // If release was successful, process waiting requests
                    if let Ok(Response::Released) = response {
                        state.process_waiting_requests(reservation_id);
                    }

                    response
                }
            }
        })
    }
}

#[derive(Clone)]
struct RestaurantService {
    reservations: Arc<Mutex<HashMap<u32, u32>>>, // reservation_id -> table_id
    next_reservation_id: Arc<Mutex<u32>>,
}

impl RestaurantService {
    fn new() -> Self {
        Self {
            reservations: Arc::new(Mutex::new(HashMap::new())),
            next_reservation_id: Arc::new(Mutex::new(1)),
        }
    }
}

impl Service<Request> for RestaurantService {
    type Response = Response;
    type Error = RestaurantError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let reservations = self.reservations.clone();
        let next_reservation_id = self.next_reservation_id.clone();

        Box::pin(async move {
            match req {
                Request::Reserve {
                    table_id,
                    client_id,
                } => {
                    // Check if the table is already reserved
                    if reservations
                        .lock()
                        .unwrap()
                        .values()
                        .any(|&id| id == table_id)
                    {
                        Err(RestaurantError::TableAlreadyReserved { id: table_id })
                    } else {
                        let reservation_id = {
                            let mut id = next_reservation_id.lock().unwrap();
                            let current = *id;
                            *id += 1;
                            current
                        };

                        let mut reservations = reservations.lock().unwrap();
                        reservations.insert(reservation_id, table_id);
                        info!(
                            "Reserving table {} for client {} with ID {}",
                            table_id, client_id, reservation_id
                        );
                        Ok(Response::Reserved { reservation_id })
                    }
                }
                Request::Release { reservation_id } => {
                    let mut reservations = reservations.lock().unwrap();
                    if !reservations.contains_key(&reservation_id) {
                        warn!(
                            "Attempted to release non-existent reservation {}",
                            reservation_id
                        );
                        Err(RestaurantError::ReservationNotFound { reservation_id })
                    } else {
                        reservations.remove(&reservation_id);
                        info!("Released reservation {}", reservation_id);
                        Ok(Response::Released)
                    }
                }
            }
        })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let mut svc = ServiceBuilder::new()
        .layer(ReservationLayer::new())
        .service(RestaurantService::new());

    let invalid_release = svc.call(Request::Release { reservation_id: 1 }); // Should fail
    let reserve1 = svc.call(Request::Reserve {
        table_id: 1,
        client_id: 1,
    });
    let release1 = svc.call(Request::Release { reservation_id: 1 });

    let reserve2 = svc.call(Request::Reserve {
        table_id: 2,
        client_id: 2,
    });
    let mut svc_clone = svc.clone();
    let delayed_release2 = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        svc_clone.call(Request::Release { reservation_id: 2 }).await // Should trigger reserve3
    });

    let reserve3 = svc.call(Request::Reserve {
        table_id: 2,
        client_id: 3,
    }); // Should wait until release2 is done

    println!("{:?}", invalid_release.await);
    println!("{:?}", reserve1.await);
    println!("{:?}", release1.await);
    println!("{:?}", reserve2.await);
    println!("{:?}", reserve3.await);
    println!("{:?}", delayed_release2.await.unwrap());
}

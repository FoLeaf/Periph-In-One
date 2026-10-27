//! Typed requests shared by controller capabilities; no domain payloads here.
use crate::{BackendExecutor, BackendOperation, ErrorKind, HardwareError};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

/// A scheduled operation. Dropping it cancels work that has not entered I/O.
pub struct OperationRequest<T> {
    receiver: Receiver<Result<T, HardwareError>>,
    active: Arc<AtomicBool>,
}

impl<T> OperationRequest<T> {
    pub fn wait(self, timeout: Duration) -> Result<T, HardwareError> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    HardwareError::new(ErrorKind::Timeout, "capability operation timed out")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    HardwareError::new(ErrorKind::Unavailable, "capability operation was cancelled")
                }
            })?
    }

    pub fn try_take(&self) -> Option<Result<T, HardwareError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(HardwareError::new(
                ErrorKind::Unavailable,
                "capability operation was cancelled",
            ))),
        }
    }

    pub(crate) fn ready(result: Result<T, HardwareError>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = sender.send(result);
        Self {
            receiver,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<T> Drop for OperationRequest<T> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

struct ControllerState<C: ?Sized> {
    controller: Mutex<Option<Arc<C>>>,
    execution: Mutex<()>,
    valid: AtomicBool,
}

pub(crate) struct ControllerClient<C: ?Sized> {
    state: Arc<ControllerState<C>>,
    executor: Option<Arc<dyn BackendExecutor>>,
}

impl<C: ?Sized> Clone for ControllerClient<C> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            executor: self.executor.clone(),
        }
    }
}

impl<C: ?Sized + Send + Sync + 'static> ControllerClient<C> {
    pub(crate) fn new(controller: Arc<C>) -> Self {
        Self {
            state: Arc::new(ControllerState {
                controller: Mutex::new(Some(controller)),
                execution: Mutex::new(()),
                valid: AtomicBool::new(true),
            }),
            executor: None,
        }
    }

    pub(crate) fn routed(&self, executor: Arc<dyn BackendExecutor>) -> Self {
        Self {
            state: self.state.clone(),
            executor: Some(executor),
        }
    }

    pub(crate) fn close(&self) {
        self.state.valid.store(false, Ordering::Release);
        if let Ok(mut controller) = self.state.controller.lock() {
            controller.take();
        }
    }

    pub(crate) fn request<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&C) -> Result<T, HardwareError> + Send + 'static,
    ) -> OperationRequest<T> {
        if !self.state.valid.load(Ordering::Acquire) {
            return OperationRequest::ready(Err(expired()));
        }
        let controller = match self.state.controller.lock() {
            Ok(controller) => controller.clone(),
            Err(_) => {
                return OperationRequest::ready(Err(HardwareError::new(
                    ErrorKind::Unavailable,
                    "capability controller lock poisoned",
                )));
            }
        };
        let Some(controller) = controller else {
            return OperationRequest::ready(Err(expired()));
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(true));
        let request = OperationRequest {
            receiver,
            active: active.clone(),
        };
        let state = self.state.clone();
        let executor = self.executor.clone();
        let run: BackendOperation = Box::new(move |_| {
            let current = || {
                state.valid.load(Ordering::Acquire)
                    && active.load(Ordering::Acquire)
                    && executor
                        .as_ref()
                        .is_none_or(|executor| executor.is_current())
            };
            let result = (|| {
                let _guard = state.execution.lock().map_err(|_| {
                    HardwareError::new(ErrorKind::Unavailable, "capability execution lock poisoned")
                })?;
                if !current() {
                    return Err(expired());
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    operation(&controller)
                }))
                .unwrap_or_else(|_| {
                    Err(HardwareError::new(
                        ErrorKind::Unavailable,
                        "capability operation panicked",
                    ))
                });
                if current() { result } else { Err(expired()) }
            })();
            let _ = sender.send(result);
        });
        if let Some(executor) = &self.executor {
            if let Err(error) = executor.enqueue(run) {
                return OperationRequest::ready(Err(error));
            }
        } else {
            run(&mut ());
        }
        request
    }
}

fn expired() -> HardwareError {
    HardwareError::new(ErrorKind::Unavailable, "capability operation has expired")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Queue(Mutex<Vec<BackendOperation>>);
    impl BackendExecutor for Queue {
        fn enqueue(&self, operation: BackendOperation) -> Result<(), HardwareError> {
            self.0.lock().unwrap().push(operation);
            Ok(())
        }
        fn is_current(&self) -> bool {
            true
        }
    }
    impl Queue {
        fn run(&self) {
            let operations = std::mem::take(&mut *self.0.lock().unwrap());
            for operation in operations {
                operation(&mut ());
            }
        }
    }

    #[test]
    fn dropped_timed_out_and_closed_requests_never_enter_io() {
        let queue = Arc::new(Queue::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let client = ControllerClient::new(calls.clone()).routed(queue.clone());
        let request = client.request(|calls| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        drop(request);
        let request = client.request(|calls| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(
            request.wait(Duration::ZERO).unwrap_err().kind,
            ErrorKind::Timeout
        );
        let request = client.request(|calls| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        client.close();
        queue.run();
        assert_eq!(
            request.wait(Duration::from_secs(1)).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn in_flight_results_are_rejected_after_close() {
        let queue = Arc::new(Queue::default());
        let client = ControllerClient::new(Arc::new(())).routed(queue.clone());
        let other = client.clone();
        let request = client.request(move |_| {
            other.close();
            Ok(42)
        });
        queue.run();
        assert_eq!(
            request.wait(Duration::from_secs(1)).unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }

    #[test]
    fn queued_reads_are_nonblocking_and_worker_survives_controller_panic() {
        let queue = Arc::new(Queue::default());
        let client = ControllerClient::new(Arc::new(())).routed(queue.clone());
        let broken = client.request::<()>(|_| panic!("test controller panic"));
        let valid = client.request(|_| Ok(42));
        assert!(valid.try_take().is_none());
        queue.run();
        assert_eq!(
            broken.wait(Duration::from_secs(1)).unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(valid.wait(Duration::from_secs(1)).unwrap(), 42);
    }
}

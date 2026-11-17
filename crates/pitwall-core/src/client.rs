//! Async client for the daemon socket, shared by the CLI and the GUI.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::proto::{ClientFrame, Push, Request, Response, ServerFrame};
use crate::wire;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<ClientFrame>,
    pending: Pending,
    next: Arc<AtomicU64>,
}

impl Client {
    /// Connect and spawn reader/writer tasks on the current tokio runtime.
    /// Push events arrive on the returned receiver.
    pub async fn connect(path: &Path) -> Result<(Client, mpsc::UnboundedReceiver<Push>)> {
        let stream = UnixStream::connect(path).await?;
        let (mut rd, mut wr) = stream.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientFrame>();
        let (push_tx, push_rx) = mpsc::unbounded_channel::<Push>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if wire::write_frame(&mut wr, &frame).await.is_err() {
                    break;
                }
            }
        });

        let pend = pending.clone();
        tokio::spawn(async move {
            loop {
                match wire::read_frame::<_, ServerFrame>(&mut rd).await {
                    Ok(Some(ServerFrame::Reply { req, body })) => {
                        if let Some(tx) = pend.lock().unwrap().remove(&req) {
                            let _ = tx.send(body);
                        }
                    }
                    Ok(Some(ServerFrame::Push(p))) => {
                        if push_tx.send(p).is_err() {
                            // Receiver dropped; keep serving replies.
                        }
                    }
                    _ => break,
                }
            }
            // Connection gone: fail all in-flight requests.
            pend.lock().unwrap().clear();
        });

        Ok((
            Client {
                tx,
                pending,
                next: Arc::new(AtomicU64::new(1)),
            },
            push_rx,
        ))
    }

    /// Send a request and wait for its reply. `Response::Error` becomes `Err`.
    pub async fn call(&self, body: Request) -> Result<Response> {
        let req = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(req, tx);
        self.tx
            .send(ClientFrame { req, body })
            .map_err(|_| anyhow!("daemon connection closed"))?;
        match rx.await {
            Ok(Response::Error(e)) => Err(anyhow!(e)),
            Ok(r) => Ok(r),
            Err(_) => Err(anyhow!("daemon connection closed")),
        }
    }

    /// Fire-and-forget (used for keystrokes, where latency matters more than the ack).
    pub fn send(&self, body: Request) {
        let req = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(ClientFrame { req, body });
    }
}

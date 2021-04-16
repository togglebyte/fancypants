use std::sync::{Arc, RwLock};
use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::oneshot;

use crate::{Message, Frame};

pub type Tx = Sender<(Message, oneshot::Sender<()>)>;
pub type Rx = Receiver<(Message, oneshot::Sender<()>)>;

// -----------------------------------------------------------------------------
//     - Client id -
// -----------------------------------------------------------------------------
#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy)]
pub struct ClientId(pub u64);

pub struct Clients {
    inner: HashMap<ClientId, Tx>,
}

impl Clients {
    pub fn new() -> Self {
        Self { inner: HashMap::new() }
    }
}

// -----------------------------------------------------------------------------
//     - Start a server -
// -----------------------------------------------------------------------------
pub async fn serve() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5555").await?;
    let mut clients = Arc::new(RwLock::new(Clients::new()));
    let mut next_id = 0;

    loop {
        if let Ok((socket, _)) = listener.accept().await {
            let (r, w) = socket.into_split();
            let (tx, rx) = mpsc::channel(10);

            let client_id = ClientId(next_id);
            next_id += 1;

            match clients.write() {
                Ok(mut c) => drop(c.inner.insert(client_id, tx)),
                Err(_) => continue,
            }

            tokio::spawn(read(r, client_id, clients.clone()));
            tokio::spawn(write(w, rx));
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
//     - Read -
// -----------------------------------------------------------------------------
async fn read(mut socket: impl AsyncReadExt + Unpin, client_id: ClientId, clients: Arc<RwLock<Clients>>) -> Result<()> {
    let mut frame = Frame::empty();

    loop {
        let should_remove = match frame.read(&mut socket).await {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => {
                eprintln!("{:?}", e);
                true
            }
        };

        // Cleanup if the connection is closed
        if should_remove {
            match clients.write() {
                Ok(mut clients) => drop(clients.inner.remove(&client_id)),
                Err(_) => {},
            }
            break;
        }

        if let Some(message) = frame.try_msg::<Message>() {
            eprintln!("Message: {:?}", message);

            let sender = match clients.read() {
                Ok(clients) => clients.inner.get(&client_id).unwrap().clone(),
                _ => continue,
            };

            eprintln!("have sender");
            let (tx, rx) = oneshot::channel();
            sender.send((message, tx)).await;
            rx.await?;
            eprintln!("Message received by client");
            eprintln!("{:?}", client_id);
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
//     - Write -
// -----------------------------------------------------------------------------
async fn write(mut socket: impl AsyncWriteExt + Unpin, mut rx: Rx) -> Result<()> {
    loop {
        if let Some((message, tx)) = rx.recv().await {
            eprintln!("Received the message");
            socket.write(&message.payload).await;
            tx.send(());
        }
    }
}

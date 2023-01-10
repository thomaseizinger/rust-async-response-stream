use std::time::Duration;
use futures::io::Cursor;
use asynchronous_codec::{Framed, LinesCodec};
use futures::channel::oneshot;
use futures_util::FutureExt;
use async_response_stream::Receiving;

#[test]
fn smoke() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let future = Receiving::new(Framed::new(Cursor::new(&mut buffer), LinesCodec));
    let (message, slot, response) = future.now_or_never().unwrap().unwrap();

    assert_eq!(message, "hello\n");

    slot.fill("world\n".to_owned());
    response.now_or_never().unwrap().unwrap();

    assert_eq!(buffer, b"hello\nworld\n");
}

#[tokio::test]
async fn runtime_driven() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let (message, slot, response) = Receiving::new(Framed::new(Cursor::new(buffer), LinesCodec)).await.unwrap();

    assert_eq!(message, "hello\n");

    let (rx, tx) = oneshot::channel();

    tokio::spawn(async move {
        let cursor = response.await.unwrap();

        rx.send(cursor.into_inner()).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await; // simulate computation of response
    slot.fill("world\n".to_owned());
    assert_eq!(tx.await.unwrap(), b"hello\nworld\n");
}
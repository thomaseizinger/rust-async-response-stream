use async_response_stream::{Error, Receiving};
use asynchronous_codec::LinesCodec;
use futures::channel::oneshot;
use futures::io::Cursor;
use futures_util::FutureExt;
use std::time::Duration;

#[test]
fn smoke() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let future = Receiving::new(Cursor::new(&mut buffer), LinesCodec, Duration::from_secs(1));
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

    let (message, slot, response) =
        Receiving::new(Cursor::new(buffer), LinesCodec, Duration::from_secs(1))
            .await
            .unwrap();

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

#[tokio::test]
async fn timeout() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"\n");

    let (_, _, response) =
        Receiving::new(Cursor::new(buffer), LinesCodec, Duration::from_millis(10))
            .await
            .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await; // delay response beyond timeout

    assert!(matches!(response.await.unwrap_err(), Error::Timeout));
}

#[tokio::test]
async fn close_after_send() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"hello\n");

    let (_, slot, response) = Receiving::new(
        Cursor::new(&mut buffer),
        LinesCodec,
        Duration::from_millis(10),
    )
    .await
    .unwrap();

    slot.fill("world\n".to_owned());
    response.close_after_send().await.unwrap();

    assert_eq!(buffer, b"hello\nworld\n");

    // TODO: How can we assert that closing works properly?
}

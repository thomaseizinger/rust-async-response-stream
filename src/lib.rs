use std::{io, future::Future, pin::Pin, sync::{Arc, Mutex}, task::{Context, Poll, Waker}, mem};
use asynchronous_codec::{Decoder, Encoder, Framed};
use futures::{
    AsyncRead,
    AsyncWrite,
    StreamExt,
    SinkExt,
};

pub struct Receiving<S, C> {
    framed: Option<Framed<S, C>>,
}

pub struct Responding<S, C> where C: Encoder {
    inner: RespondingState<S, C>,
}

#[derive(Debug)]
pub enum Error<Enc> {
    Recv(Enc),
    Send(Enc),
}

pub struct Slot<Res> {
    shared: Arc<Mutex<Shared<Res>>>,
}

impl<S, C> Receiving<S, C> {
    pub fn new(framed: Framed<S, C>) -> Self {
        Self {
            framed: Some(framed)
        }
    }
}

impl<S, C, Req, Res, E> Future for Receiving<S, C> where S: AsyncRead + AsyncWrite + Unpin, C: Encoder<Item=Res, Error=E> + Decoder<Item=Req, Error=E>, E: From<io::Error> {
    type Output = Result<(Req, Slot<Res>, Responding<S, C>), Error<E>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let framed = self.framed.as_mut().expect("to not be polled after completion");
        let req = futures::ready!(framed.poll_next_unpin(cx).map_err(Error::Recv)?).ok_or_else(|| Error::Recv(E::from(io::Error::from(io::ErrorKind::UnexpectedEof))))?;

        let shared = Arc::new(Mutex::new(Shared::default()));

        let fut = Responding {
            inner: RespondingState::Sending { framed: self.framed.take().expect("to not be polled after completion"), shared: shared.clone() }
        };
        let slot = Slot {
            shared,
        };

        Poll::Ready(Ok((req, slot, fut)))
    }
}

impl<S, C, Req, Res, E> Future for Responding<S, C> where S: AsyncRead + AsyncWrite + Unpin, C: Encoder<Item=Res, Error=E> + Decoder<Item=Req, Error=E> {
    type Output = Result<S, Error<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RespondingState::Poisoned) {
                RespondingState::Sending { mut framed, shared } => {
                    match framed.poll_ready_unpin(cx).map_err(Error::Send)? {
                        Poll::Ready(()) => {}
                        Poll::Pending => {
                            this.inner = RespondingState::Sending { framed, shared };
                            return Poll::Pending;
                        }
                    }

                    let mut guard = shared.lock().unwrap();

                    let response = match guard.message.take() {
                        Some(response) => response,
                        None => {
                            guard.waker = Some(cx.waker().clone());
                            drop(guard);

                            this.inner = RespondingState::Sending { framed, shared };
                            return Poll::Pending;
                        }
                    };

                    framed.start_send_unpin(response).map_err(Error::Send)?;
                    this.inner = RespondingState::Flushing { framed };
                }
                RespondingState::Flushing { mut framed } => match framed.poll_flush_unpin(cx).map_err(Error::Recv)? {
                    Poll::Ready(()) => {
                        let stream = framed.into_parts().io;

                        return Poll::Ready(Ok(stream));
                    }
                    Poll::Pending => {}
                },
                RespondingState::Poisoned => unreachable!()
            }
        }
    }
}

impl<Res> Slot<Res> {
    pub fn fill(self, res: Res) {
        let mut guard = self.shared.lock().unwrap();

        guard.message = Some(res);
        if let Some(waker) = guard.waker.take() {
            waker.wake();
        }
    }
}

enum RespondingState<S, C> where C: Encoder {
    Sending {
        framed: Framed<S, C>,
        shared: Arc<Mutex<Shared<C::Item>>>,
    },
    Flushing {
        framed: Framed<S, C>,
    },
    Poisoned,
}

struct Shared<M> {
    message: Option<M>,
    waker: Option<Waker>,
}

impl<M> Default for Shared<M> {
    fn default() -> Self {
        Self {
            message: None,
            waker: None,
        }
    }
}

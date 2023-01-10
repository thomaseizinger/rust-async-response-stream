use asynchronous_codec::{Decoder, Encoder, Framed};
use futures::FutureExt;
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};
use futures_timer::Delay;
use std::marker::PhantomData;
use std::time::Duration;
use std::{
    future::Future,
    io, mem,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

pub struct Receiving<S, C> {
    inner: ReceivingState<S, C>,
}

pub struct Responding<S, C, B>
where
    C: Encoder,
{
    inner: RespondingState<S, C>,
    behaviour: PhantomData<B>,
}

pub enum ReturnStream {}

pub enum CloseStream {}

#[derive(Debug)]
pub enum Error<Enc> {
    Recv(Enc),
    Send(Enc),
    Timeout,
}

pub struct Slot<Res> {
    shared: Arc<Mutex<Shared<Res>>>,
}

impl<S, C> Receiving<S, C> {
    pub fn new(framed: Framed<S, C>, timeout: Duration) -> Self {
        Self {
            inner: ReceivingState::Receiving {
                framed,
                timeout: Delay::new(timeout),
            },
        }
    }
}

impl<S, C> Responding<S, C, ReturnStream>
where
    C: Encoder,
{
    /// Reconfigure this future to close the stream after the message has been sent instead of returning it.
    pub fn close_after_send(self) -> Responding<S, C, CloseStream> {
        Responding {
            inner: self.inner,
            behaviour: Default::default(),
        }
    }
}

impl<S, C, Req, Res, E> Future for Receiving<S, C>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
    E: From<io::Error>,
{
    type Output = Result<(Req, Slot<Res>, Responding<S, C, ReturnStream>), Error<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, ReceivingState::Poisoned) {
                ReceivingState::Receiving {
                    mut framed,
                    mut timeout,
                } => {
                    match timeout.poll_unpin(cx) {
                        Poll::Ready(()) => {
                            return Poll::Ready(Err(Error::Timeout));
                        }
                        Poll::Pending => {}
                    }

                    let request = match framed.poll_next_unpin(cx).map_err(Error::Recv)? {
                        Poll::Ready(Some(request)) => request,
                        Poll::Ready(None) => {
                            return Poll::Ready(Err(Error::Recv(E::from(io::Error::from(
                                io::ErrorKind::UnexpectedEof,
                            )))));
                        }
                        Poll::Pending => {
                            this.inner = ReceivingState::Receiving { framed, timeout };
                            return Poll::Pending;
                        }
                    };

                    let shared = Arc::new(Mutex::new(Shared::default()));

                    let fut = Responding {
                        inner: RespondingState::Sending(Sending {
                            framed,
                            shared: shared.clone(),
                            timeout,
                        }),
                        behaviour: Default::default(),
                    };
                    let slot = Slot { shared };

                    return Poll::Ready(Ok((request, slot, fut)));
                }
                ReceivingState::Poisoned => {
                    unreachable!()
                }
            }
        }
    }
}

impl<S, C, Req, Res, E> Future for Responding<S, C, ReturnStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
{
    type Output = Result<S, Error<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RespondingState::Poisoned) {
                RespondingState::Sending(mut sending) => match sending.poll(cx)? {
                    Poll::Ready(()) => {
                        this.inner = RespondingState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Pending => {
                        this.inner = RespondingState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RespondingState::Flushing { mut framed } => {
                    match framed.poll_flush_unpin(cx).map_err(Error::Recv)? {
                        Poll::Ready(()) => {
                            let stream = framed.into_parts().io;

                            return Poll::Ready(Ok(stream));
                        }
                        Poll::Pending => {
                            this.inner = RespondingState::Flushing { framed };
                            return Poll::Pending;
                        }
                    }
                }
                RespondingState::Closing { .. } => {
                    unreachable!("we don't go into `Closing` in the `ReturnStream` behaviour")
                }
                RespondingState::Poisoned => unreachable!(),
            }
        }
    }
}

impl<S, C, Req, Res, E> Future for Responding<S, C, CloseStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
{
    type Output = Result<(), Error<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.inner, RespondingState::Poisoned) {
                RespondingState::Sending(mut sending) => match sending.poll(cx)? {
                    Poll::Ready(()) => {
                        this.inner = RespondingState::Flushing {
                            framed: sending.framed,
                        };
                    }
                    Poll::Pending => {
                        this.inner = RespondingState::Sending(sending);
                        return Poll::Pending;
                    }
                },
                RespondingState::Flushing { mut framed } => {
                    match framed.poll_flush_unpin(cx).map_err(Error::Recv)? {
                        Poll::Ready(()) => {
                            this.inner = RespondingState::Closing { framed };
                        }
                        Poll::Pending => {
                            this.inner = RespondingState::Flushing { framed };
                            return Poll::Pending;
                        }
                    }
                }
                RespondingState::Closing { mut framed } => {
                    return match framed.poll_close_unpin(cx).map_err(Error::Recv)? {
                        Poll::Ready(()) => Poll::Ready(Ok(())),
                        Poll::Pending => {
                            this.inner = RespondingState::Closing { framed };
                            Poll::Pending
                        }
                    }
                }
                RespondingState::Poisoned => unreachable!(),
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

enum ReceivingState<S, C> {
    Receiving {
        framed: Framed<S, C>,
        timeout: Delay,
    },
    Poisoned,
}

enum RespondingState<S, C>
where
    C: Encoder,
{
    Sending(Sending<S, C>),
    Flushing { framed: Framed<S, C> },
    Closing { framed: Framed<S, C> },
    Poisoned,
}

struct Sending<S, C>
where
    C: Encoder,
{
    framed: Framed<S, C>,
    shared: Arc<Mutex<Shared<C::Item>>>,
    timeout: Delay,
}

impl<S, C, Req, Res, E> Sending<S, C>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Encoder<Item = Res, Error = E> + Decoder<Item = Req, Error = E>,
{
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error<E>>> {
        match self.timeout.poll_unpin(cx) {
            Poll::Ready(()) => {
                return Poll::Ready(Err(Error::Timeout));
            }
            Poll::Pending => {}
        }

        futures::ready!(self.framed.poll_ready_unpin(cx).map_err(Error::Send))?;

        let mut guard = self.shared.lock().unwrap();

        let response = match guard.message.take() {
            Some(response) => response,
            None => {
                guard.waker = Some(cx.waker().clone());
                drop(guard);

                return Poll::Pending;
            }
        };

        self.framed
            .start_send_unpin(response)
            .map_err(Error::Send)?;

        Poll::Ready(Ok(()))
    }
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

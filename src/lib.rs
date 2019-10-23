//! Tokio codec for use with bincode
//!
//! This crate provides a `bincode` based codec that can be used with
//! tokio's `Framed`, `FramedRead`, and `FramedWrite`.
//!
//! # Example
//!
//! ```
//! # use futures::{Stream, Sink};
//! # use tokio::io::{AsyncRead, AsyncWrite};
//! # use tokio::codec::Framed;
//! # use serde::{Serialize, Deserialize};
//! # use tokio_bincode::BinCodec;
//! # use serde_derive::{Serialize, Deserialize};
//! # fn sd<'a>(transport: impl AsyncRead + AsyncWrite) {
//! #[derive(Serialize, Deserialize)]
//! struct MyProtocol;
//!
//! // Create the codec based on your custom protocol
//! let codec = BinCodec::<MyProtocol>::new();
//!
//! // Frame the transport with the codec to produce a stream/sink
//! let (sink, stream) = Framed::new(transport, codec).split();
//! # }
//! ```
//!
//! # Features
//!
//! This crate provides a single feature, `big_data`, which enables large amounts of data
//! to be encoded by prepending the length of the data to the data itself,
//! using tokio's `LengthDelimitedCodec`.
//!
//! This functionality is optional because it might affect performance.

#![deny(missing_docs, missing_debug_implementations)]

use bincode::Config;
use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use std::{fmt, marker::PhantomData};
use tokio::codec::{Decoder, Encoder};

#[cfg(feature = "big_data")]
use tokio::codec::length_delimited::{Builder, LengthDelimitedCodec};

/// Bincode based codec for use with `tokio-codec`
///
/// # Note
///
/// Optionally depends on [`LengthDelimitedCodec`](https://docs.rs/tokio/0.1/tokio/codec/length_delimited/struct.LengthDelimitedCodec.html)
/// when `big_data` feature is enabled
pub struct BinCodec<T> {
    #[cfg(feature = "big_data")]
    lower: LengthDelimitedCodec,
    config: Config,
    _pd: PhantomData<T>,
}

impl<T> BinCodec<T> {
    /// Provides a bincode based codec
    pub fn new() -> Self { Self::default() }

    /// Provides a bincode based codec from the bincode config
    #[cfg(not(feature = "big_data"))]
    pub fn with_config(config: Config) -> Self { BinCodec { config, _pd: PhantomData } }

    /// Provides a bincode based codec from the bincode config and a `LengthDelimitedCodec` builder
    #[cfg(feature = "big_data")]
    pub fn with_config(config: Config, builder: &mut Builder) -> Self {
        BinCodec { lower: builder.new_codec(), config, _pd: PhantomData }
    }
}

impl<T> Default for BinCodec<T> {
    #[inline]
    fn default() -> Self {
        let config = bincode::config();
        BinCodec::with_config(
            config,
            #[cfg(feature = "big_data")]
            &mut Builder::new(),
        )
    }
}

impl<T> Decoder for BinCodec<T>
where
    for<'de> T: Deserialize<'de>,
{
    type Error = bincode::Error;
    type Item = T;

    #[cfg(feature = "big_data")]
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Ok(if let Some(buf) = self.lower.decode(src)? {
            Some(self.config.deserialize(&buf)?)
        } else {
            None
        })
    }

    #[cfg(not(feature = "big_data"))]
    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if !buf.is_empty() {
            let mut reader = reader::Reader::new(&buf[..]);
            let message = self.config.deserialize_from(&mut reader)?;
            let amount = reader.amount();
            buf.split_to(amount);
            Ok(Some(message))
        } else {
            Ok(None)
        }
    }
}

impl<T> Encoder for BinCodec<T>
where
    T: Serialize,
{
    type Error = bincode::Error;
    type Item = T;

    #[cfg(feature = "big_data")]
    fn encode(&mut self, item: T, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = self.config.serialize(&item)?;
        self.lower.encode(bytes.into(), dst)?;
        Ok(())
    }

    #[cfg(not(feature = "big_data"))]
    fn encode(&mut self, item: T, buf: &mut BytesMut) -> Result<(), Self::Error> {
        use bytes::BufMut;
        let size = self.config.serialized_size(&item)?;
        buf.reserve(size as usize);
        let message = self.config.serialize(&item)?;
        buf.put(&message[..]);
        Ok(())
    }
}

impl<T> fmt::Debug for BinCodec<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.debug_struct("BinCodec").finish() }
}

#[cfg(not(feature = "big_data"))]
mod reader {
    use tokio::{io, prelude::Read};

    #[derive(Debug)]
    pub struct Reader<'buf> {
        buf: &'buf [u8],
        amount: usize,
    }

    impl<'buf> Reader<'buf> {
        pub fn new(buf: &'buf [u8]) -> Self { Reader { buf, amount: 0 } }

        pub fn amount(&self) -> usize { self.amount }
    }

    impl<'buf, 'a> Read for &'a mut Reader<'buf> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let bytes_read = self.buf.read(buf)?;
            self.amount += bytes_read;
            Ok(bytes_read)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{Future, Sink, Stream};
    use serde::{Deserialize, Serialize};
    use serde_derive::{Deserialize, Serialize};
    use std::{net::SocketAddr, thread::JoinHandle};
    use tokio::{
        codec::Framed,
        net::{TcpListener, TcpStream},
        runtime::current_thread,
    };


    fn start_server<T>(addr: SocketAddr) -> JoinHandle<()>
    where
        T: Serialize + 'static + for<'de> Deserialize<'de>,
    {
        let echo = TcpListener::bind(&addr).unwrap();

        std::thread::spawn(move || {
            current_thread::run(
                echo.incoming()
                    .map_err(bincode::Error::from)
                    .take(1)
                    .for_each(|stream| {
                        let (w, r) = Framed::new(stream, BinCodec::<T>::new()).split();
                        r.forward(w).map(|_| ())
                    })
                    .map_err(|_| ()),
            )
        })
    }

    #[test]
    fn it_works() {
        #[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
        enum Mock {
            One,
            Two,
        }

        let addr = SocketAddr::new("127.0.0.1".parse().unwrap(), 15151);
        let jh = start_server::<Mock>(addr);

        let client = TcpStream::connect(&addr).wait().unwrap();
        let client = Framed::new(client, BinCodec::<Mock>::new());

        let client = client.send(Mock::One).wait().unwrap();

        let (got, client) = match client.into_future().wait() {
            Ok(x) => x,
            Err((e, _)) => panic!("[Mock::One]> Error during deserialize: {:?}", e),
        };

        assert_eq!(got, Some(Mock::One));

        let client = client.send(Mock::Two).wait().unwrap();

        let (got, client) = match client.into_future().wait() {
            Ok(x) => x,
            Err((e, _)) => panic!("[Mock::Two]> Error during deserialize: {:?}", e),
        };

        assert_eq!(got, Some(Mock::Two));

        drop(client);
        jh.join().unwrap();
    }

    #[test]
    #[cfg(feature = "big_data")]
    fn big_data() {
        #[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
        enum Mock {
            One(Vec<u8>),
            Two,
        }

        let addr = SocketAddr::new("127.0.0.1".parse().unwrap(), 15152);
        let jh = start_server::<Mock>(addr);

        let client = TcpStream::connect(&addr).wait().unwrap();
        let client = Framed::new(client, BinCodec::<Mock>::new());
        let data = Mock::One(vec![0; 1_000_000]);
        let client = client.send(data.clone()).wait().unwrap();

        let (got, client) = match client.into_future().wait() {
            Ok(x) => x,
            Err((e, _)) => panic!("[Mock::One]> Error during deserialize: {:?}", e),
        };

        assert_eq!(got, Some(data));

        let data = Mock::Two;
        let client = client.send(data.clone()).wait().unwrap();

        let (got, client) = match client.into_future().wait() {
            Ok(x) => x,
            Err((e, _)) => panic!("[Mock::Two]> Error during deserialize: {:?}", e),
        };

        assert_eq!(got, Some(data));

        drop(client);
        jh.join().unwrap();
    }
}

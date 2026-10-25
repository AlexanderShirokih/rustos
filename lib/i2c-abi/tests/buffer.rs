#![allow(clippy::cast_possible_truncation, clippy::expect_used)]

use std::thread;

use i2c_abi::{TransferBuf, TransferError, i2c_device};
use ipc::{CallError, ReceivedResources};

struct Device;

#[allow(clippy::unused_async_trait_impl)]
impl<F: ReceivedResources> i2c_device::Service<F> for Device {
    async fn write(&self, _frame: &F, _data: &[u8]) -> Result<(), TransferError> {
        Err(TransferError::NoAck)
    }

    async fn read(&self, _frame: &F, count: u16) -> Result<TransferBuf, TransferError> {
        Ok(TransferBuf((0..count).map(|byte| byte as u8).collect()))
    }

    async fn write_read(
        &self,
        _frame: &F,
        data: &[u8],
        count: u16,
    ) -> Result<TransferBuf, TransferError> {
        let mut buf = data.to_vec();
        buf.resize(data.len() + usize::from(count), 0xA5);
        Ok(TransferBuf(buf))
    }
}

#[test]
fn read_bytes_arrive_in_order() {
    let (channel, listener) = ipc_test::pair();
    let answers = thread::scope(|scope| {
        scope.spawn(move || {
            let device = Device;
            for _ in 0..3 {
                listener
                    .serve(|frame| {
                        ipc_test::complete(i2c_device::dispatch(
                            &device,
                            ipc::OwnedIncoming::new(frame).expect("frame fits"),
                        ))
                    })
                    .expect("dispatch ok");
            }
        });
        let client = i2c_device::Client::new(channel);
        (
            client.read(4).wait(),
            client.write_read(&[0x78], 2).wait(),
            client.write(&[1]).wait(),
        )
    });

    assert_eq!(answers.0.expect("read").0, &[0, 1, 2, 3]);
    assert_eq!(answers.1.expect("write_read").0, &[0x78, 0xA5, 0xA5]);
    assert_eq!(answers.2, Err(CallError::Remote(TransferError::NoAck)));
}

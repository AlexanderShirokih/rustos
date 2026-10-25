use core::future::{Future, ready};

use ipc::{AsyncChannel, Channel, Incoming, Outgoing, Timeout, Unsent, wire::IpcError};

use crate::{MockChannel, MockResources};

impl AsyncChannel for MockChannel {
    fn send<'b>(
        &'b self,
        frame: Outgoing<'b, MockResources>,
        wait: Timeout,
    ) -> impl Future<Output = Result<(), Unsent<'b, MockResources>>> + 'b {
        ready(Channel::send(self, frame, wait))
    }

    fn call<'b>(
        &'b self,
        frame: Outgoing<'b, MockResources>,
        wait: Timeout,
    ) -> impl Future<Output = Result<Incoming<'b, MockResources>, IpcError>> + 'b {
        ready(Channel::call(self, frame, wait))
    }
}

use my_tcp_sockets::{
    socket_reader::{ReadingTcpContractFail, SocketReader},
    TcpPayload, TcpSocketSerializer,
};

use crate::MyNoSqlTcpContract;

pub struct MyNoSqlReaderTcpSerializer {}

impl MyNoSqlReaderTcpSerializer {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl TcpSocketSerializer<MyNoSqlTcpContract> for MyNoSqlReaderTcpSerializer {
    const PING_PACKET_IS_SINGLETON: bool = true;

    fn serialize<'s>(&self, contract: &'s MyNoSqlTcpContract) -> TcpPayload<'s> {
        contract.serialize()
    }

    fn get_ping(&self) -> MyNoSqlTcpContract {
        MyNoSqlTcpContract::Ping
    }

    async fn deserialize<TSocketReader: Send + Sync + 'static + SocketReader>(
        &mut self,
        socket_reader: &mut TSocketReader,
    ) -> Result<MyNoSqlTcpContract, ReadingTcpContractFail> {
        MyNoSqlTcpContract::deserialize(socket_reader).await
    }
}

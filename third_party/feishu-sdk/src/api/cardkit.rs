use crate::Client;
use crate::client::OperationBuilder;
use crate::generated::ops;

pub struct CardKitApi<'a> {
    client: &'a Client,
}

impl<'a> CardKitApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn card_create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::cardkit::v1::card::CREATE)
    }

    pub fn card_update(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::cardkit::v1::card::UPDATE)
    }

    pub fn card_batch_update(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::cardkit::v1::card::BATCH_UPDATE)
    }

    pub fn card_settings(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::cardkit::v1::card::SETTINGS)
    }

    pub fn card_id_convert(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::cardkit::v1::card::ID_CONVERT)
    }

    pub fn card_element_create(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::cardkit::v1::card_element::CREATE)
    }
}

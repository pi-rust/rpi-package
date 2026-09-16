use serde::{Deserialize, Serialize};

use super::ApiEnvelope;
use crate::Client;
use crate::client::OperationBuilder;
use crate::core::{DownloadedFile, Error, MultipartForm, RequestOptions};
use crate::generated::ops;
use crate::utils::guess_content_type;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImFileUploadResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImImageUploadResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImFileUploadBody {
    pub file_type: String,
    pub file_name: String,
    pub duration: Option<u64>,
    pub content_type: Option<String>,
}

impl ImFileUploadBody {
    pub fn new(file_type: impl Into<String>, file_name: impl Into<String>) -> Self {
        Self {
            file_type: file_type.into(),
            file_name: file_name.into(),
            duration: None,
            content_type: None,
        }
    }

    pub fn duration(mut self, duration: u64) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    fn to_multipart_form(&self, file_bytes: Vec<u8>) -> MultipartForm {
        let mut form = MultipartForm::new()
            .text("file_type", self.file_type.clone())
            .text("file_name", self.file_name.clone());
        if let Some(duration) = self.duration {
            form = form.text("duration", duration.to_string());
        }

        let content_type = self
            .content_type
            .clone()
            .or_else(|| guess_content_type(&self.file_name).map(str::to_string));
        match content_type {
            Some(content_type) => form.file_with_content_type(
                "file",
                self.file_name.clone(),
                content_type,
                file_bytes,
            ),
            None => form.file("file", self.file_name.clone(), file_bytes),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImImageUploadBody {
    pub image_type: String,
    pub file_name: String,
    pub content_type: Option<String>,
}

impl ImImageUploadBody {
    pub fn new(image_type: impl Into<String>, file_name: impl Into<String>) -> Self {
        Self {
            image_type: image_type.into(),
            file_name: file_name.into(),
            content_type: None,
        }
    }

    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    fn to_multipart_form(&self, image_bytes: Vec<u8>) -> MultipartForm {
        let form = MultipartForm::new().text("image_type", self.image_type.clone());
        let content_type = self
            .content_type
            .clone()
            .or_else(|| guess_content_type(&self.file_name).map(str::to_string));
        match content_type {
            Some(content_type) => form.file_with_content_type(
                "image",
                self.file_name.clone(),
                content_type,
                image_bytes,
            ),
            None => form.file("image", self.file_name.clone(), image_bytes),
        }
    }
}

pub struct ImV1FileApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1FileApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::file::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::file::GET)
    }

    pub async fn upload_file(
        &self,
        body: &ImFileUploadBody,
        file_bytes: impl Into<Vec<u8>>,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ImFileUploadResponseData>, Error> {
        let response = self
            .create()
            .body_multipart(body.to_multipart_form(file_bytes.into()))
            .options(options)
            .send()
            .await?;
        response.json()
    }

    pub async fn download_file(
        &self,
        file_key: impl Into<String>,
        options: RequestOptions,
    ) -> Result<DownloadedFile, Error> {
        let response = self
            .get()
            .path_param("file_key", file_key.into())
            .options(options)
            .send()
            .await?;
        Ok(response.downloaded_file())
    }
}

pub struct ImV1ImageApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1ImageApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::image::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::image::GET)
    }

    pub async fn upload_image(
        &self,
        body: &ImImageUploadBody,
        image_bytes: impl Into<Vec<u8>>,
        options: RequestOptions,
    ) -> Result<ApiEnvelope<ImImageUploadResponseData>, Error> {
        let response = self
            .create()
            .body_multipart(body.to_multipart_form(image_bytes.into()))
            .options(options)
            .send()
            .await?;
        response.json()
    }

    pub async fn download_image(
        &self,
        image_key: impl Into<String>,
        options: RequestOptions,
    ) -> Result<DownloadedFile, Error> {
        let response = self
            .get()
            .path_param("image_key", image_key.into())
            .options(options)
            .send()
            .await?;
        Ok(response.downloaded_file())
    }
}

pub struct ImV1PinApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1PinApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::pin::CREATE)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::pin::LIST)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::pin::DELETE)
    }
}

pub struct ImV1ReactionApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1ReactionApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::message_reaction::CREATE)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::message_reaction::LIST)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::message_reaction::DELETE)
    }
}

pub struct ImV1ThreadApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1ThreadApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn forward(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::im::v1::thread::FORWARD)
    }
}

pub struct ImV1MessageResourceApi<'a> {
    client: &'a Client,
}

impl<'a> ImV1MessageResourceApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn get(
        &self,
        message_id: impl Into<String>,
        file_key: impl Into<String>,
    ) -> OperationBuilder<'a> {
        self.client
            .operation(ops::im::v1::message_resource::GET)
            .path_param("message_id", message_id.into())
            .path_param("file_key", file_key.into())
    }

    pub async fn download(
        &self,
        message_id: impl Into<String>,
        file_key: impl Into<String>,
        query: Vec<(String, String)>,
        options: RequestOptions,
    ) -> Result<DownloadedFile, Error> {
        let mut builder = self.get(message_id, file_key).options(options);
        for (key, value) in query {
            builder = builder.query_param(key, value);
        }
        let response = builder.send().await?;
        Ok(response.downloaded_file())
    }
}

pub struct ContactV3DepartmentApi<'a> {
    client: &'a Client,
}

impl<'a> ContactV3DepartmentApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::department::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::department::GET)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::department::LIST)
    }

    pub fn update(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::department::UPDATE)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::department::DELETE)
    }
}

pub struct ContactV3GroupApi<'a> {
    client: &'a Client,
}

impl<'a> ContactV3GroupApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::group::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::group::GET)
    }

    pub fn simplelist(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::group::SIMPLELIST)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::group::PATCH)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::group::DELETE)
    }
}

pub struct ContactV3UnitApi<'a> {
    client: &'a Client,
}

impl<'a> ContactV3UnitApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::unit::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::unit::GET)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::unit::LIST)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::unit::PATCH)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::contact::v3::unit::DELETE)
    }
}

pub struct DriveV1FolderApi<'a> {
    client: &'a Client,
}

impl<'a> DriveV1FolderApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::drive::v1::file::CREATE_FOLDER)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::drive::v1::file::LIST)
    }
}

pub struct DriveV1PermissionApi<'a> {
    client: &'a Client,
}

impl<'a> DriveV1PermissionApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn member_create(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_member::CREATE)
    }

    pub fn member_list(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_member::LIST)
    }

    pub fn member_update(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_member::UPDATE)
    }

    pub fn member_delete(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_member::DELETE)
    }

    pub fn public_get(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_public::GET)
    }

    pub fn public_patch(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::drive::v1::permission_public::PATCH)
    }
}

pub struct CalendarV4CalendarEventApi<'a> {
    client: &'a Client,
}

impl<'a> CalendarV4CalendarEventApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::calendar::v4::calendar_event::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::calendar::v4::calendar_event::GET)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::calendar::v4::calendar_event::LIST)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::calendar::v4::calendar_event::PATCH)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::calendar::v4::calendar_event::DELETE)
    }
}

pub struct DocxV1DocumentApi<'a> {
    client: &'a Client,
}

impl<'a> DocxV1DocumentApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document::GET)
    }

    pub fn raw_content(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document::RAW_CONTENT)
    }
}

pub struct DocxV1BlockApi<'a> {
    client: &'a Client,
}

impl<'a> DocxV1BlockApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document_block::GET)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document_block::LIST)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::docx::v1::document_block::PATCH)
    }

    pub fn batch_update(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::docx::v1::document_block::BATCH_UPDATE)
    }
}

pub struct SheetsV3SpreadsheetApi<'a> {
    client: &'a Client,
}

impl<'a> SheetsV3SpreadsheetApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::sheets::v3::spreadsheet::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::sheets::v3::spreadsheet::GET)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::sheets::v3::spreadsheet::PATCH)
    }
}

pub struct SheetsV3SheetApi<'a> {
    client: &'a Client,
}

impl<'a> SheetsV3SheetApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn find(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::sheets::v3::spreadsheet_sheet::FIND)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::sheets::v3::spreadsheet_sheet::GET)
    }

    pub fn query(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::sheets::v3::spreadsheet_sheet::QUERY)
    }

    pub fn replace(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::sheets::v3::spreadsheet_sheet::REPLACE)
    }
}

pub struct BitableV1AppApi<'a> {
    client: &'a Client,
}

impl<'a> BitableV1AppApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app::GET)
    }

    pub fn update(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app::UPDATE)
    }
}

pub struct BitableV1TableApi<'a> {
    client: &'a Client,
}

impl<'a> BitableV1TableApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app_table::CREATE)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app_table::LIST)
    }

    pub fn patch(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app_table::PATCH)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client.operation(ops::bitable::v1::app_table::DELETE)
    }
}

pub struct BitableV1RecordApi<'a> {
    client: &'a Client,
}

impl<'a> BitableV1RecordApi<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    pub fn create(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::bitable::v1::app_table_record::CREATE)
    }

    pub fn get(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::bitable::v1::app_table_record::GET)
    }

    pub fn list(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::bitable::v1::app_table_record::LIST)
    }

    pub fn update(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::bitable::v1::app_table_record::UPDATE)
    }

    pub fn delete(&self) -> OperationBuilder<'a> {
        self.client
            .operation(ops::bitable::v1::app_table_record::DELETE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MultipartFieldValue;

    #[test]
    fn im_file_upload_body_builds_form() {
        let body = ImFileUploadBody::new("stream", "clip.mp4")
            .duration(3_000)
            .content_type("video/mp4");
        let form = body.to_multipart_form(vec![1, 2, 3]);

        assert_eq!(form.fields.len(), 4);
        assert!(matches!(
            form.fields.last().map(|field| &field.value),
            Some(MultipartFieldValue::File(file))
                if file.file_name == "clip.mp4"
                    && file.content_type.as_deref() == Some("video/mp4")
        ));
    }

    #[test]
    fn im_image_upload_body_builds_form() {
        let body = ImImageUploadBody::new("message", "avatar.png");
        let form = body.to_multipart_form(vec![1, 2, 3]);

        assert_eq!(form.fields.len(), 2);
        assert!(matches!(
            form.fields.last().map(|field| &field.value),
            Some(MultipartFieldValue::File(file)) if file.file_name == "avatar.png"
        ));
    }
}

pub mod all_services;
mod approval_v4;
mod auth_v3;
mod calendar_v4_calendar;
mod cardkit;
mod common;
mod contact_v3_user;
mod drive_v1_file;
mod ext;
mod generated_models;
mod im_v1_chat;
mod im_v1_message;
mod service_wrappers;

pub use approval_v4::{
    ApprovalV4Api, ApprovalV4ApprovalApi, ApprovalV4InstanceApi, ApprovalV4TaskApi,
    ApproveTaskBody, CancelInstanceBody, CreateApprovalBody, CreateInstanceBody, GetInstanceQuery,
    RejectTaskBody,
};
pub use auth_v3::{
    AppAccessTokenResp, AuthV3Api, MarketplaceAppAccessTokenReq, MarketplaceTenantAccessTokenReq,
    ResendAppTicketReq, ResendAppTicketResp, SelfBuiltAppAccessTokenReq,
    SelfBuiltTenantAccessTokenReq, TenantAccessTokenResp,
};
pub use calendar_v4_calendar::{
    CalendarInfo, CalendarV4CalendarApi, CreateCalendarBody, GetCalendarQuery, ListCalendarQuery,
    UpdateCalendarBody,
};
pub use cardkit::CardKitApi;
pub use common::ApiEnvelope;
pub use contact_v3_user::{
    ContactUserInfo, ContactV3UserApi, GetContactUserQuery, ListContactUserQuery,
};
pub use drive_v1_file::{
    CreateFolderBody, DownloadFileQuery, DriveV1FileApi, FileInfo, ListFileQuery,
    MoveFileToTrashBody, UploadFileBody, UploadFileQuery,
};
pub use ext::ExtApi;
pub use generated_models::*;
pub use im_v1_chat::{
    ChatInfo, CreateChatBody, CreateChatQuery, DeleteChatQuery, GetChatQuery, ImV1ChatApi,
    ListChatQuery,
};
pub use im_v1_message::{
    ImV1MessageApi, ListMessageQuery, MessageInfo, ReplyMessageBody, ReplyMessageQuery,
    SendMessageBody, SendMessageQuery,
};
pub use service_wrappers::{
    BitableV1AppApi, BitableV1RecordApi, BitableV1TableApi, CalendarV4CalendarEventApi,
    ContactV3DepartmentApi, ContactV3GroupApi, ContactV3UnitApi, DocxV1BlockApi, DocxV1DocumentApi,
    DriveV1FolderApi, DriveV1PermissionApi, ImFileUploadBody, ImFileUploadResponseData,
    ImImageUploadBody, ImImageUploadResponseData, ImV1FileApi, ImV1ImageApi,
    ImV1MessageResourceApi, ImV1PinApi, ImV1ReactionApi, ImV1ThreadApi, SheetsV3SheetApi,
    SheetsV3SpreadsheetApi,
};

use crate::Client;

impl Client {
    pub fn auth_v3(&self) -> AuthV3Api<'_> {
        AuthV3Api::new(self)
    }

    pub fn im_v1_chat(&self) -> ImV1ChatApi<'_> {
        ImV1ChatApi::new(self)
    }

    pub fn im_v1_message(&self) -> ImV1MessageApi<'_> {
        ImV1MessageApi::new(self)
    }

    pub fn contact_v3_user(&self) -> ContactV3UserApi<'_> {
        ContactV3UserApi::new(self)
    }

    pub fn drive_v1_file(&self) -> DriveV1FileApi<'_> {
        DriveV1FileApi::new(self)
    }

    pub fn drive_v1_folder(&self) -> DriveV1FolderApi<'_> {
        DriveV1FolderApi::new(self)
    }

    pub fn drive_v1_permission(&self) -> DriveV1PermissionApi<'_> {
        DriveV1PermissionApi::new(self)
    }

    pub fn calendar_v4(&self) -> CalendarV4CalendarApi<'_> {
        CalendarV4CalendarApi::new(self)
    }

    pub fn calendar_v4_calendar_event(&self) -> CalendarV4CalendarEventApi<'_> {
        CalendarV4CalendarEventApi::new(self)
    }

    pub fn approval_v4(&self) -> ApprovalV4Api<'_> {
        ApprovalV4Api::new(self)
    }

    pub fn approval_v4_approval(&self) -> ApprovalV4ApprovalApi<'_> {
        ApprovalV4ApprovalApi::new(self)
    }

    pub fn approval_v4_instance(&self) -> ApprovalV4InstanceApi<'_> {
        ApprovalV4InstanceApi::new(self)
    }

    pub fn approval_v4_task(&self) -> ApprovalV4TaskApi<'_> {
        ApprovalV4TaskApi::new(self)
    }

    pub fn im_v1_file(&self) -> ImV1FileApi<'_> {
        ImV1FileApi::new(self)
    }

    pub fn im_v1_image(&self) -> ImV1ImageApi<'_> {
        ImV1ImageApi::new(self)
    }

    pub fn im_v1_pin(&self) -> ImV1PinApi<'_> {
        ImV1PinApi::new(self)
    }

    pub fn im_v1_reaction(&self) -> ImV1ReactionApi<'_> {
        ImV1ReactionApi::new(self)
    }

    pub fn im_v1_message_resource(&self) -> ImV1MessageResourceApi<'_> {
        ImV1MessageResourceApi::new(self)
    }

    pub fn im_v1_thread(&self) -> ImV1ThreadApi<'_> {
        ImV1ThreadApi::new(self)
    }

    pub fn contact_v3_department(&self) -> ContactV3DepartmentApi<'_> {
        ContactV3DepartmentApi::new(self)
    }

    pub fn contact_v3_group(&self) -> ContactV3GroupApi<'_> {
        ContactV3GroupApi::new(self)
    }

    pub fn contact_v3_unit(&self) -> ContactV3UnitApi<'_> {
        ContactV3UnitApi::new(self)
    }

    pub fn docx_v1_document(&self) -> DocxV1DocumentApi<'_> {
        DocxV1DocumentApi::new(self)
    }

    pub fn docx_v1_block(&self) -> DocxV1BlockApi<'_> {
        DocxV1BlockApi::new(self)
    }

    pub fn sheets_v3_spreadsheet(&self) -> SheetsV3SpreadsheetApi<'_> {
        SheetsV3SpreadsheetApi::new(self)
    }

    pub fn sheets_v3_sheet(&self) -> SheetsV3SheetApi<'_> {
        SheetsV3SheetApi::new(self)
    }

    pub fn bitable_v1_app(&self) -> BitableV1AppApi<'_> {
        BitableV1AppApi::new(self)
    }

    pub fn bitable_v1_table(&self) -> BitableV1TableApi<'_> {
        BitableV1TableApi::new(self)
    }

    pub fn bitable_v1_record(&self) -> BitableV1RecordApi<'_> {
        BitableV1RecordApi::new(self)
    }

    pub fn cardkit(&self) -> CardKitApi<'_> {
        CardKitApi::new(self)
    }

    pub fn ext(&self) -> ExtApi<'_> {
        ExtApi::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Config;
    use crate::generated::ops;

    #[test]
    fn all_services_wrapper_builds_expected_operation() {
        let client = Client::new(Config::builder("app", "secret").build()).expect("client");
        let builder = all_services::im::v1::chat::get(&client, "oc_123")
            .query_param("user_id_type", "open_id");

        let (operation_id, path_params, query, ..) = builder.into_inner();
        assert_eq!(operation_id, ops::im::v1::chat::GET);
        assert_eq!(
            path_params.get("chat_id").map(String::as_str),
            Some("oc_123")
        );
        assert_eq!(
            query,
            vec![("user_id_type".to_string(), "open_id".to_string())]
        );
    }

    #[test]
    fn all_services_wrapper_handles_multiple_path_params() {
        let client = Client::new(Config::builder("app", "secret").build()).expect("client");
        let builder = all_services::admin::v1::badge_grant::get(&client, "badge_1", "grant_2");
        let (operation_id, path_params, ..) = builder.into_inner();
        assert_eq!(operation_id, ops::admin::v1::badge_grant::GET);
        assert_eq!(
            path_params.get("badge_id").map(String::as_str),
            Some("badge_1")
        );
        assert_eq!(
            path_params.get("grant_id").map(String::as_str),
            Some("grant_2")
        );
    }
}

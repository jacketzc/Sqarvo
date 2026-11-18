//! 锁定共享档案 RPC 的方法、字段号与可选字段语义。

use prost::Message as _;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{FileDescriptorProto, FileDescriptorSet};

fn profile_descriptor() -> FileDescriptorProto {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-descriptor.bin"));
    FileDescriptorSet::decode(bytes.as_slice())
        .expect("生成的描述符可解析")
        .file
        .into_iter()
        .find(|file| file.name() == "services/v1/profile.proto")
        .expect("档案契约参与生成")
}

#[test]
fn profile_rpc_names_and_types_are_frozen() {
    let file = profile_descriptor();
    assert_eq!(file.package(), "sqarvo.services.v1");
    let [service] = file.service.as_slice() else {
        panic!("档案契约只有一个服务");
    };
    assert_eq!(service.name(), "ConnectionProfileService");
    let methods = service
        .method
        .iter()
        .map(|method| {
            assert!(!method.client_streaming() && !method.server_streaming());
            (method.name(), method.input_type(), method.output_type())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        [
            (
                "ListProfiles",
                ".sqarvo.common.v1.Empty",
                ".sqarvo.services.v1.ListProfilesResponse"
            ),
            (
                "GetProfile",
                ".sqarvo.services.v1.GetProfileRequest",
                ".sqarvo.services.v1.ConnectionProfile"
            ),
            (
                "SaveProfile",
                ".sqarvo.services.v1.SaveProfileRequest",
                ".sqarvo.services.v1.ConnectionProfile"
            ),
            (
                "DeleteProfile",
                ".sqarvo.services.v1.DeleteProfileRequest",
                ".sqarvo.common.v1.Empty"
            ),
            (
                "GetMcpAccessPolicy",
                ".sqarvo.common.v1.Empty",
                ".sqarvo.services.v1.McpAccessPolicyResponse"
            ),
            (
                "SetMcpAccessPolicy",
                ".sqarvo.services.v1.SetMcpAccessPolicyRequest",
                ".sqarvo.services.v1.McpAccessPolicyResponse"
            ),
        ]
    );
}

#[test]
fn profile_fields_and_presence_are_frozen() {
    use Type::{Bool, Message, String, Uint64};

    type Field = (i32, &'static str, Type, bool);
    let file = profile_descriptor();
    let expected: &[(&str, &[Field])] = &[
        (
            "ConnectionProfile",
            &[
                (1, "profile_id", String, false),
                (2, "revision", Uint64, false),
                (3, "name", String, false),
                (4, "params", Message, false),
                (5, "file_path", String, false),
                (6, "display_name", String, true),
                (7, "last_used", String, true),
            ],
        ),
        (
            "ListProfilesResponse",
            &[
                (1, "profiles", Message, false),
                (2, "revision", Uint64, false),
            ],
        ),
        ("GetProfileRequest", &[(1, "profile_id", String, false)]),
        (
            "SaveProfileRequest",
            &[
                (1, "profile", Message, false),
                (2, "expected_revision", Uint64, true),
            ],
        ),
        (
            "DeleteProfileRequest",
            &[
                (1, "profile_id", String, false),
                (2, "expected_revision", Uint64, false),
            ],
        ),
        (
            "McpAccessPolicyResponse",
            &[
                (1, "enabled", Bool, false),
                (2, "read_only", Bool, false),
                (3, "revision", Uint64, false),
            ],
        ),
        (
            "SetMcpAccessPolicyRequest",
            &[(1, "enabled", Bool, true), (2, "read_only", Bool, true)],
        ),
    ];
    assert_eq!(file.message_type.len(), expected.len());
    for (name, expected_fields) in expected {
        let message = file
            .message_type
            .iter()
            .find(|item| item.name() == *name)
            .unwrap();
        let fields = message
            .field
            .iter()
            .map(|field| {
                (
                    field.number(),
                    field.name(),
                    field.r#type(),
                    field.proto3_optional(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(&fields, expected_fields, "{name}");
        for field in &message.field {
            let (type_name, label) = match (*name, field.name()) {
                ("ListProfilesResponse", "profiles") => {
                    (".sqarvo.services.v1.ConnectionProfile", Label::Repeated)
                }
                ("SaveProfileRequest", "profile") => {
                    (".sqarvo.services.v1.ConnectionProfile", Label::Optional)
                }
                ("ConnectionProfile", "params") => {
                    (".sqarvo.services.v1.ConnectionParams", Label::Optional)
                }
                _ => ("", Label::Optional),
            };
            assert_eq!(field.type_name(), type_name, "{name}.{}", field.name());
            assert_eq!(field.label(), label, "{name}.{}", field.name());
        }
    }
}

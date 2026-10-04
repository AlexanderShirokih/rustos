use platform_abi::{DEVICE_ID_MAX, DeviceId};

#[test]
fn a_device_id_is_an_absolute_path_of_names_within_the_limit() {
    let longest = format!("/{}", "n".repeat(DEVICE_ID_MAX - 1));
    let past = format!("/{}", "n".repeat(DEVICE_ID_MAX));
    let valid = ["/pl031@9010000", "/soc/serial@1000", longest.as_str()];
    let invalid = [
        "",
        "/",
        "soc",
        "/soc/",
        "//soc",
        "/soc//serial",
        past.as_str(),
    ];

    let accepted = valid.map(|path| DeviceId::new(path).map(|id| id.as_str().to_owned()));
    let refused = invalid.map(|path| DeviceId::new(path).is_none());

    assert_eq!(accepted, valid.map(|path| Some(path.to_owned())));
    assert_eq!(refused, [true; 7]);
}

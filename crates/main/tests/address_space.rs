mod common;

use main::sched::AddressSpace;

use crate::common::MockAddressSpaceFactory;

#[test]
fn kernel_variant_has_no_root() {
    let kernel = AddressSpace::kernel();
    assert!(kernel.handle().is_none());
    assert!(kernel.mapper().is_none());
}

#[test]
fn user_variant_exposes_root() {
    let factory = MockAddressSpaceFactory::new();
    let user = AddressSpace::new_user(&factory).expect("create user AS");
    let handle = user.handle().expect("user AS must have a handle");
    assert_eq!(
        handle.root.as_usize(),
        MockAddressSpaceFactory::BASE_ROOT_PA
    );
    assert!(user.mapper().is_some());
    assert_eq!(factory.created(), 1);
}

#[test]
fn two_user_address_spaces_have_distinct_roots() {
    let factory = MockAddressSpaceFactory::new();
    let a = AddressSpace::new_user(&factory).expect("create A");
    let b = AddressSpace::new_user(&factory).expect("create B");
    assert_ne!(a.handle().map(|h| h.root), b.handle().map(|h| h.root));
    assert_eq!(factory.created(), 2);
}

#[test]
fn drop_user_address_space_releases_root() {
    let factory = MockAddressSpaceFactory::new();
    {
        let _user = AddressSpace::new_user(&factory).expect("create user");
        assert_eq!(factory.released(), 0);
    }
    assert_eq!(factory.released(), 1);
}

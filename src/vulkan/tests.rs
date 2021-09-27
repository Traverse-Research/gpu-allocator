#[test]
fn test_memory_packing() {
    let bits = super::pack_memory_bits(63, 27);
    assert_eq!(super::unpack_memory_type_index(bits), 27);
    assert_eq!(super::unpack_memory_block_index(bits), 63);

    let bits = super::pack_memory_bits(0x7ffffff, 31);

    // NB: bits should be fully populated.
    assert_eq!(bits, !0);

    assert_eq!(super::unpack_memory_block_index(bits), 0x7ffffff);

    assert_eq!(super::unpack_memory_type_index(bits), 31);
}

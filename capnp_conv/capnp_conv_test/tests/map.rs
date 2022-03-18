use std::collections::BTreeMap;

use proptest::{prelude::ProptestConfig, test_runner::FileFailurePersistence};
use test_strategy::proptest;

use capnp_conv::{
    CapnpConvError, CorrespondingCapnp, FromCapnpBytes, ReadCapnp, ToCapnpBytes, WriteCapnp,
};

#[allow(unused)]
mod test_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/test_capnp.rs"));
}

#[derive(derive_more::From, Eq, PartialEq, Debug)]
struct Map<K, V>(BTreeMap<K, V>);

impl<K, V> CorrespondingCapnp for Map<K, V>
where
    K: CorrespondingCapnp,
    V: CorrespondingCapnp,
{
    type Type =
        test_capnp::map::Owned<<K as CorrespondingCapnp>::Type, <V as CorrespondingCapnp>::Type>;
}

impl<'a, K, V> ReadCapnp<'a> for Map<K, V>
where
    K: for<'c> ReadCapnp<'c> + std::cmp::Ord,
    V: for<'c> ReadCapnp<'c>,
    <K as CorrespondingCapnp>::Type: Copy,
    <V as CorrespondingCapnp>::Type: Copy,
{
    fn read_capnp(
        reader: &<Self::Type as ::capnp::traits::Owned<'a>>::Reader,
    ) -> Result<Self, CapnpConvError> {
        let mut btree_map = BTreeMap::new();
        for item_reader in reader.get_entries()? {
            let key = K::read_capnp(&item_reader.get_key()?)?;
            let value = V::read_capnp(&item_reader.get_value()?)?;
            btree_map.insert(key, value);
        }
        Ok(Map(btree_map))
    }
}

impl<'a, K, V> WriteCapnp<'a> for Map<K, V>
where
    K: for<'c> WriteCapnp<'c>,
    V: for<'c> WriteCapnp<'c>,
    <K as CorrespondingCapnp>::Type: Copy,
    <V as CorrespondingCapnp>::Type: Copy,
{
    fn write_capnp(&self, writer: &mut <Self::Type as ::capnp::traits::Owned<'a>>::Builder) {
        fn usize_to_u32(num: usize) -> Option<u32> {
            if num > 0xffffffff_usize {
                None
            } else {
                Some(num as u32)
            }
        }
        let mut entries_list_builder = writer
            .reborrow()
            .init_entries(usize_to_u32(self.0.len()).unwrap());
        for (index, (key, value)) in self.0.iter().enumerate() {
            let mut entry_builder = entries_list_builder
                .reborrow()
                .get(usize_to_u32(index).unwrap());
            key.write_capnp(&mut entry_builder.reborrow().init_key());
            value.write_capnp(&mut entry_builder.reborrow().init_value());
        }
    }
}

#[test]
fn unit_map_round_trip() {
    let unit_map: Map<(), ()> = vec![((), ())]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into();
    let data = unit_map.to_packed_capnp_bytes();
    let unit_map2 = <Map<(), ()>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(unit_map, unit_map2);
}

#[test]
fn empty_unit_map_round_trip() {
    let unit_map: Map<(), ()> = Map(BTreeMap::new());
    let data = unit_map.to_packed_capnp_bytes();
    let unit_map2 = <Map<(), ()>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(unit_map, unit_map2);
}

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("regressions"))),
        ..Default::default()
    }
}

#[proptest(proptest_config())]
fn u8_map_round_trip(btree_map: BTreeMap<u8, u8>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn u16_map_round_trip(btree_map: BTreeMap<u16, u16>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn u32_map_round_trip(btree_map: BTreeMap<u32, u32>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn u64_map_round_trip(btree_map: BTreeMap<u64, u64>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn i8_map_round_trip(btree_map: BTreeMap<i8, i8>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn i16_map_round_trip(btree_map: BTreeMap<i16, i16>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn i32_map_round_trip(btree_map: BTreeMap<i32, i32>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn i64_map_round_trip(btree_map: BTreeMap<i64, i64>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

// Note that floating point numbers don't impl Ord (because of NaN), so we can't use a BTreeMap
#[proptest(proptest_config())]
fn f32_map_round_trip(btree_map: BTreeMap<i64, f32>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn f64_map_round_trip(btree_map: BTreeMap<i64, f64>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}

#[proptest(proptest_config())]
fn bool_map_round_trip(btree_map: BTreeMap<u8, bool>) {
    let map = Map(btree_map);
    let data = map.to_packed_capnp_bytes();
    let map2 = <Map<_, _>>::from_packed_capnp_bytes(&data).unwrap();
    assert_eq!(map, map2);
}
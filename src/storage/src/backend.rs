// Copyright 2020 Alex Dukhno
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use kernel::{SystemError, SystemResult};
use std::{collections::HashMap, fmt::Debug};

pub type Result<T, E> = std::result::Result<T, E>;
pub type Row = (Key, Values);
pub type Key = Vec<u8>;
pub type Values = Vec<u8>;
pub type ReadCursor = Box<dyn Iterator<Item = Result<Row, SystemError>>>;

#[derive(Debug, PartialEq)]
pub struct NamespaceAlreadyExists;
#[derive(Debug, PartialEq)]
pub struct NamespaceDoesNotExist;

#[derive(Debug, PartialEq)]
pub enum CreateObjectError {
    NamespaceDoesNotExist,
    ObjectAlreadyExists,
}

#[derive(Debug, PartialEq)]
pub enum DropObjectError {
    NamespaceDoesNotExist,
    ObjectDoesNotExist,
}

#[derive(Debug, PartialEq)]
pub enum OperationOnObjectError {
    NamespaceDoesNotExist,
    ObjectDoesNotExist,
}

pub trait BackendStorage {
    type ErrorMapper: StorageErrorMapper;

    fn create_namespace(&mut self, namespace: &str) -> SystemResult<Result<(), NamespaceAlreadyExists>>;

    fn drop_namespace(&mut self, namespace: &str) -> SystemResult<Result<(), NamespaceDoesNotExist>>;

    fn create_object(&mut self, namespace: &str, object_name: &str) -> SystemResult<Result<(), CreateObjectError>>;

    fn drop_object(&mut self, namespace: &str, object_name: &str) -> SystemResult<Result<(), DropObjectError>>;

    fn write(
        &mut self,
        namespace: &str,
        object_name: &str,
        values: Vec<Row>,
    ) -> SystemResult<Result<usize, OperationOnObjectError>>;

    fn read(&self, namespace: &str, object_name: &str) -> SystemResult<Result<ReadCursor, OperationOnObjectError>>;

    fn delete(
        &mut self,
        namespace: &str,
        object_name: &str,
        keys: Vec<Key>,
    ) -> SystemResult<Result<usize, OperationOnObjectError>>;
}

pub trait StorageErrorMapper {
    type Error;

    fn map(error: Self::Error) -> kernel::SystemError;
}

pub struct SledErrorMapper;

impl StorageErrorMapper for SledErrorMapper {
    type Error = sled::Error;

    fn map(error: Self::Error) -> SystemError {
        match error {
            sled::Error::CollectionNotFound(system_file) => SystemError::unrecoverable(format!(
                "System file [{}] can't be found",
                String::from_utf8(system_file.to_vec()).expect("name of system file")
            )),
            sled::Error::Unsupported(operation) => {
                SystemError::unrecoverable(format!("Unsupported operation [{}] was used on Sled", operation))
            }
            sled::Error::Corruption { at, bt: cause } => {
                if let Some(at) = at {
                    SystemError::unrecoverable_with_cause(format!("Sled encountered corruption at {}", at), cause)
                } else {
                    SystemError::unrecoverable_with_cause("Sled encountered corruption".to_owned(), cause)
                }
            }
            sled::Error::ReportableBug(description) => {
                SystemError::unrecoverable(format!("Sled encountered reportable BUG: {}", description))
            }
            sled::Error::Io(error) => SystemError::io(error),
        }
    }
}

#[derive(Default)]
pub struct SledBackendStorage {
    namespaces: HashMap<String, sled::Db>,
}

impl BackendStorage for SledBackendStorage {
    type ErrorMapper = SledErrorMapper;

    fn create_namespace(&mut self, namespace: &str) -> SystemResult<Result<(), NamespaceAlreadyExists>> {
        if self.namespaces.contains_key(namespace) {
            Ok(Err(NamespaceAlreadyExists))
        } else {
            match sled::Config::default().temporary(true).open() {
                Ok(database) => {
                    self.namespaces.insert(namespace.to_owned(), database);
                    Ok(Ok(()))
                }
                Err(error) => Err(Self::ErrorMapper::map(error)),
            }
        }
    }

    fn drop_namespace(&mut self, namespace: &str) -> SystemResult<Result<(), NamespaceDoesNotExist>> {
        match self.namespaces.remove(namespace) {
            Some(namespace) => {
                drop(namespace);
                Ok(Ok(()))
            }
            None => Ok(Err(NamespaceDoesNotExist)),
        }
    }

    fn create_object(&mut self, namespace: &str, object_name: &str) -> SystemResult<Result<(), CreateObjectError>> {
        match self.namespaces.get(namespace) {
            Some(namespace) => {
                if namespace.tree_names().contains(&(object_name.into())) {
                    Ok(Err(CreateObjectError::ObjectAlreadyExists))
                } else {
                    match namespace.open_tree(object_name) {
                        Ok(_object) => Ok(Ok(())),
                        Err(error) => Err(Self::ErrorMapper::map(error)),
                    }
                }
            }
            None => Ok(Err(CreateObjectError::NamespaceDoesNotExist)),
        }
    }

    fn drop_object(&mut self, namespace: &str, object_name: &str) -> SystemResult<Result<(), DropObjectError>> {
        match self.namespaces.get(namespace) {
            Some(namespace) => match namespace.drop_tree(object_name.as_bytes()) {
                Ok(true) => Ok(Ok(())),
                Ok(false) => Ok(Err(DropObjectError::ObjectDoesNotExist)),
                Err(error) => Err(Self::ErrorMapper::map(error)),
            },
            None => Ok(Err(DropObjectError::NamespaceDoesNotExist)),
        }
    }

    fn write(
        &mut self,
        namespace: &str,
        object_name: &str,
        rows: Vec<Row>,
    ) -> SystemResult<Result<usize, OperationOnObjectError>> {
        match self.namespaces.get(namespace) {
            Some(namespace) => {
                if namespace.tree_names().contains(&(object_name.into())) {
                    match namespace.open_tree(object_name) {
                        Ok(object) => {
                            let mut written_rows = 0;
                            for (key, values) in rows {
                                // let to_insert = values
                                //     .iter()
                                //     .map(|v| v.as_slice())
                                //     .collect::<Vec<&[u8]>>()
                                //     .join(&b'|')
                                //     .to_vec();
                                match object.insert::<sled::IVec, sled::IVec>(key.into(), values.into()) {
                                    Ok(_) => written_rows += 1,
                                    Err(error) => return Err(Self::ErrorMapper::map(error)),
                                }
                            }
                            Ok(Ok(written_rows))
                        }
                        Err(error) => Err(Self::ErrorMapper::map(error)),
                    }
                } else {
                    Ok(Err(OperationOnObjectError::ObjectDoesNotExist))
                }
            }
            None => Ok(Err(OperationOnObjectError::NamespaceDoesNotExist)),
        }
    }

    fn read(&self, namespace: &str, object_name: &str) -> SystemResult<Result<ReadCursor, OperationOnObjectError>> {
        match self.namespaces.get(namespace) {
            Some(namespace) => {
                if namespace.tree_names().contains(&(object_name.into())) {
                    match namespace.open_tree(object_name) {
                        Ok(object) => Ok(Ok(Box::new(object.iter().map(|item| {
                            match item {
                                Ok((key, values)) => Ok((
                                    key.to_vec(),
                                    values.to_vec(),
                                    // .split(|b| *b == b'|')
                                    // .map(|v| v.to_vec())
                                    // .collect::<Vec<Vec<u8>>>(),
                                )),
                                Err(error) => Err(Self::ErrorMapper::map(error)),
                            }
                        })))),
                        Err(error) => Err(Self::ErrorMapper::map(error)),
                    }
                } else {
                    Ok(Err(OperationOnObjectError::ObjectDoesNotExist))
                }
            }
            None => Ok(Err(OperationOnObjectError::NamespaceDoesNotExist)),
        }
    }

    fn delete(
        &mut self,
        namespace: &str,
        object_name: &str,
        keys: Vec<Key>,
    ) -> SystemResult<Result<usize, OperationOnObjectError>> {
        match self.namespaces.get(namespace) {
            Some(namespace) => {
                if namespace.tree_names().contains(&(object_name.into())) {
                    let mut deleted = 0;
                    match namespace.open_tree(object_name) {
                        Ok(object) => {
                            for key in keys {
                                match object.remove(key) {
                                    Ok(_) => deleted += 1,
                                    Err(error) => return Err(Self::ErrorMapper::map(error)),
                                }
                            }
                        }
                        Err(error) => return Err(Self::ErrorMapper::map(error)),
                    }
                    Ok(Ok(deleted))
                } else {
                    Ok(Err(OperationOnObjectError::ObjectDoesNotExist))
                }
            }
            None => Ok(Err(OperationOnObjectError::NamespaceDoesNotExist)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtrace::Backtrace;

    #[cfg(test)]
    mod sled_error_mapper {
        use super::*;
        use sled::DiskPtr;
        use std::io::{Error, ErrorKind};

        #[test]
        fn collection_not_found() {
            assert_eq!(
                SledErrorMapper::map(sled::Error::CollectionNotFound(sled::IVec::from("test"))),
                SystemError::unrecoverable("System file [test] can't be found".to_owned())
            )
        }

        #[test]
        fn unsupported() {
            assert_eq!(
                SledErrorMapper::map(sled::Error::Unsupported("NOT_SUPPORTED".to_owned())),
                SystemError::unrecoverable("Unsupported operation [NOT_SUPPORTED] was used on Sled".to_owned())
            )
        }

        #[test]
        fn corruption_with_position() {
            let cause = Backtrace::new();
            let at = DiskPtr::Inline(900);
            assert_eq!(
                SledErrorMapper::map(sled::Error::Corruption {
                    at: Some(at),
                    bt: cause.clone()
                }),
                SystemError::unrecoverable_with_cause(format!("Sled encountered corruption at {}", at), cause,)
            )
        }

        #[test]
        fn corruption_without_position() {
            let cause = Backtrace::new();
            assert_eq!(
                SledErrorMapper::map(sled::Error::Corruption {
                    at: None,
                    bt: cause.clone()
                }),
                SystemError::unrecoverable_with_cause("Sled encountered corruption".to_owned(), cause,)
            )
        }

        #[test]
        fn reportable_bug() {
            let description = "SOME_BUG_HERE";
            assert_eq!(
                SledErrorMapper::map(sled::Error::ReportableBug(description.to_owned())),
                SystemError::unrecoverable(format!("Sled encountered reportable BUG: {}", description))
            );
        }

        #[test]
        fn io() {
            assert_eq!(
                SledErrorMapper::map(sled::Error::Io(Error::new(ErrorKind::Other, "oh no!"))),
                SystemError::io(Error::new(ErrorKind::Other, "oh no!"))
            )
        }
    }

    #[cfg(test)]
    mod namespace {
        use super::*;

        #[test]
        fn create_namespaces_with_different_names() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage.create_namespace("namespace_1").expect("namespace created"),
                Ok(())
            );
            assert_eq!(
                storage.create_namespace("namespace_2").expect("namespace created"),
                Ok(())
            );
        }

        #[test]
        fn create_namespace_with_existing_name() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");

            assert_eq!(
                storage.create_namespace("namespace").expect("no system errors"),
                Err(NamespaceAlreadyExists)
            );
        }

        #[test]
        fn drop_namespace() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");

            assert_eq!(storage.drop_namespace("namespace").expect("no system errors"), Ok(()));
            assert_eq!(storage.create_namespace("namespace").expect("no system errors"), Ok(()));
        }

        #[test]
        fn drop_namespace_that_was_not_created() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage.drop_namespace("does_not_exists").expect("no system errors"),
                Err(NamespaceDoesNotExist)
            );
        }

        #[test]
        fn dropping_namespace_drops_objects_in_it() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");
            storage
                .create_object("namespace", "object_name_1")
                .expect("no system errors")
                .expect("object created");
            storage
                .create_object("namespace", "object_name_2")
                .expect("no system errors")
                .expect("object created");

            assert_eq!(storage.drop_namespace("namespace").expect("no system errors"), Ok(()));
            assert_eq!(
                storage.create_namespace("namespace").expect("namespace created"),
                Ok(())
            );
            assert_eq!(
                storage
                    .create_object("namespace", "object_name_1")
                    .expect("no system errors"),
                Ok(())
            );
            assert_eq!(
                storage
                    .create_object("namespace", "object_name_2")
                    .expect("no system errors"),
                Ok(())
            );
        }
    }

    #[cfg(test)]
    mod create_object {
        use super::*;

        #[test]
        fn create_objects_with_different_names() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");

            assert_eq!(
                storage
                    .create_object("namespace", "object_name_1")
                    .expect("no system errors"),
                Ok(())
            );
            assert_eq!(
                storage
                    .create_object("namespace", "object_name_2")
                    .expect("no system errors"),
                Ok(())
            );
        }

        #[test]
        fn create_object_with_the_same_name() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");

            assert_eq!(
                storage
                    .create_object("namespace", "object_name")
                    .expect("no system errors"),
                Err(CreateObjectError::ObjectAlreadyExists)
            );
        }

        #[test]
        fn create_object_with_the_same_name_in_different_namespaces() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace_1")
                .expect("no system errors")
                .expect("namespace created");
            storage
                .create_namespace("namespace_2")
                .expect("no system errors")
                .expect("namespace created");
            assert_eq!(
                storage
                    .create_object("namespace_1", "object_name")
                    .expect("no system errors"),
                Ok(())
            );
            assert_eq!(
                storage
                    .create_object("namespace_2", "object_name")
                    .expect("no system errors"),
                Ok(())
            );
        }

        #[test]
        fn create_object_in_not_existent_namespace() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage
                    .create_object("not_existent", "object_name")
                    .expect("no system errors"),
                Err(CreateObjectError::NamespaceDoesNotExist)
            );
        }
    }

    #[cfg(test)]
    mod drop_object {
        use super::*;

        #[test]
        fn drop_object() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            assert_eq!(
                storage
                    .drop_object("namespace", "object_name")
                    .expect("no system errors"),
                Ok(())
            );
            assert_eq!(
                storage
                    .create_object("namespace", "object_name")
                    .expect("no system errors"),
                Ok(())
            );
        }

        #[test]
        fn drop_not_created_object() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");
            assert_eq!(
                storage
                    .drop_object("namespace", "not_existed_object")
                    .expect("no system errors"),
                Err(DropObjectError::ObjectDoesNotExist)
            );
        }

        #[test]
        fn drop_object_in_not_existent_namespace() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage.drop_object("not_existent", "object").expect("no system errors"),
                Err(DropObjectError::NamespaceDoesNotExist)
            );
        }
    }

    #[cfg(test)]
    mod operations_on_object {
        use super::*;

        #[test]
        fn insert_row_into_object() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            assert_eq!(
                storage
                    .write("namespace", "object_name", as_rows(vec![(1u8, vec!["123"])]))
                    .expect("no system errors"),
                Ok(1)
            );

            assert_eq!(
                storage
                    .read("namespace", "object_name")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Ok(as_read_cursor(vec![(1u8, vec!["123"])]).collect())
            );
        }

        #[test]
        fn insert_many_rows_into_object() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            storage
                .write("namespace", "object_name", as_rows(vec![(1u8, vec!["123"])]))
                .expect("no system errors")
                .expect("values are written");
            storage
                .write("namespace", "object_name", as_rows(vec![(2u8, vec!["456"])]))
                .expect("no system errors")
                .expect("values are written");

            assert_eq!(
                storage
                    .read("namespace", "object_name")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Ok(as_read_cursor(vec![(1u8, vec!["123"]), (2u8, vec!["456"])]).collect())
            );
        }

        #[test]
        fn insert_into_non_existent_object() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");
            assert_eq!(
                storage
                    .write("namespace", "not_existed", as_rows(vec![(1u8, vec!["123"])],))
                    .expect("no system errors"),
                Err(OperationOnObjectError::ObjectDoesNotExist)
            );
        }

        #[test]
        fn insert_into_object_in_non_existent_namespace() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage
                    .write("not_existed", "object", as_rows(vec![(1u8, vec!["123"])],))
                    .expect("no system errors"),
                Err(OperationOnObjectError::NamespaceDoesNotExist)
            );
        }

        #[test]
        fn select_from_object_that_does_not_exist() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");
            assert_eq!(
                storage
                    .read("namespace", "not_existed")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Err(OperationOnObjectError::ObjectDoesNotExist)
            );
        }

        #[test]
        fn select_from_object_in_not_existent_namespace() {
            let storage = SledBackendStorage::default();

            assert_eq!(
                storage
                    .read("not_existed", "object")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Err(OperationOnObjectError::NamespaceDoesNotExist)
            );
        }

        #[test]
        fn delete_some_records_from_object() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            storage
                .write(
                    "namespace",
                    "object_name",
                    as_rows(vec![(1u8, vec!["123"]), (2u8, vec!["456"]), (3u8, vec!["789"])]),
                )
                .expect("no system errors")
                .expect("write occurred");

            assert_eq!(
                storage
                    .delete("namespace", "object_name", as_keys(vec![2u8]))
                    .expect("no system errors"),
                Ok(1)
            );

            assert_eq!(
                storage
                    .read("namespace", "object_name")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Ok(as_read_cursor(vec![(1u8, vec!["123"]), (3u8, vec!["789"])]).collect())
            );
        }

        #[test]
        fn delete_from_not_existed_object() {
            let mut storage = SledBackendStorage::default();

            storage
                .create_namespace("namespace")
                .expect("no system errors")
                .expect("namespace created");

            assert_eq!(
                storage
                    .delete("namespace", "not_existent", vec![])
                    .expect("no system errors"),
                Err(OperationOnObjectError::ObjectDoesNotExist)
            );
        }

        #[test]
        fn delete_from_not_existent_namespace() {
            let mut storage = SledBackendStorage::default();

            assert_eq!(
                storage
                    .delete("not existent", "object", vec![])
                    .expect("no system errors"),
                Err(OperationOnObjectError::NamespaceDoesNotExist)
            );
        }

        #[test]
        fn select_all_from_object_with_many_columns() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            storage
                .write("namespace", "object_name", as_rows(vec![(1u8, vec!["1", "2", "3"])]))
                .expect("no system errors")
                .expect("write occurred");

            assert_eq!(
                storage
                    .read("namespace", "object_name")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Ok(as_read_cursor(vec![(1u8, vec!["1", "2", "3"])]).collect())
            );
        }

        #[test]
        fn insert_multiple_rows() {
            let mut storage = SledBackendStorage::default();

            create_object(&mut storage, "namespace", "object_name");
            storage
                .write(
                    "namespace",
                    "object_name",
                    as_rows(vec![
                        (1u8, vec!["1", "2", "3"]),
                        (2u8, vec!["4", "5", "6"]),
                        (3u8, vec!["7", "8", "9"]),
                    ]),
                )
                .expect("no system errors")
                .expect("write occurred");

            assert_eq!(
                storage
                    .read("namespace", "object_name")
                    .expect("no system errors")
                    .map(|iter| iter.collect::<Vec<Result<Row, SystemError>>>()),
                Ok(as_read_cursor(vec![
                    (1u8, vec!["1", "2", "3"]),
                    (2u8, vec!["4", "5", "6"]),
                    (3u8, vec!["7", "8", "9"])
                ])
                .collect()),
            );
        }
    }

    fn create_object(storage: &mut SledBackendStorage, namespace: &str, object_name: &str) {
        storage
            .create_namespace(namespace)
            .expect("no system errors")
            .expect("namespace created");
        storage
            .create_object(namespace, object_name)
            .expect("no system errors")
            .expect("object created");
    }

    fn as_rows(items: Vec<(u8, Vec<&'static str>)>) -> Vec<Row> {
        items
            .into_iter()
            .map(|(key, values)| {
                let k = key.to_be_bytes().to_vec();
                let v = values
                    .into_iter()
                    .map(|s| s.as_bytes())
                    .collect::<Vec<&[u8]>>()
                    .join(&b'|');
                (k, v)
            })
            .collect()
    }

    fn as_keys(items: Vec<u8>) -> Vec<Key> {
        items.into_iter().map(|key| key.to_be_bytes().to_vec()).collect()
    }

    fn as_read_cursor(items: Vec<(u8, Vec<&'static str>)>) -> ReadCursor {
        Box::new(items.into_iter().map(|(key, values)| {
            let k = key.to_be_bytes().to_vec();
            let v = values
                .into_iter()
                .map(|s| s.as_bytes())
                .collect::<Vec<&[u8]>>()
                .join(&b'|');
            Ok((k, v))
        }))
    }
}

use std::vec;

use rust_extensions::date_time::DateTimeAsMicroseconds;

pub trait ExpirationIndex<TOwnedType: Clone> {
    fn get_id_as_str(&self) -> &str;
    fn to_owned(&self) -> TOwnedType;
    fn get_expiration_moment(&self) -> Option<DateTimeAsMicroseconds>;
}

pub struct ExpirationIndexItem<TOwnedType: Clone + ExpirationIndex<TOwnedType>> {
    pub moment: DateTimeAsMicroseconds,
    pub items: Vec<TOwnedType>,
}

impl<TOwnedType: Clone + ExpirationIndex<TOwnedType>> ExpirationIndexItem<TOwnedType> {
    pub fn new(moment: DateTimeAsMicroseconds, itm: TOwnedType) -> Self {
        Self {
            moment,
            items: vec![itm],
        }
    }

    pub fn remove(&mut self, key_as_str: &str) -> bool {
        self.items.retain(|f| f.get_id_as_str() != key_as_str);
        self.items.is_empty()
    }
}

pub struct ExpirationIndexContainer<TOwnedType: Clone + ExpirationIndex<TOwnedType>> {
    index: Vec<ExpirationIndexItem<TOwnedType>>,
    amount: usize,
}

impl<TOwnedType: Clone + ExpirationIndex<TOwnedType>> ExpirationIndexContainer<TOwnedType> {
    pub fn new() -> Self {
        Self {
            index: Vec::new(),
            amount: 0,
        }
    }

    fn find_index(&self, expiration_moment: DateTimeAsMicroseconds) -> Result<usize, usize> {
        self.index.binary_search_by(|itm| {
            itm.moment
                .unix_microseconds
                .cmp(&expiration_moment.unix_microseconds)
        })
    }

    pub fn add(&mut self, item: &impl ExpirationIndex<TOwnedType>) {
        let expiration_moment = item.get_expiration_moment();
        if item.get_expiration_moment().is_none() {
            return;
        }

        let expiration_moment = expiration_moment.unwrap();

        match self.find_index(expiration_moment) {
            Ok(index) => {
                self.index[index].items.push(item.to_owned());
            }
            Err(index) => {
                self.index.insert(
                    index,
                    ExpirationIndexItem::new(expiration_moment, item.to_owned()),
                );
            }
        }

        self.amount += 1;
    }

    pub fn update(
        &mut self,
        old_expires: Option<DateTimeAsMicroseconds>,
        itm: &impl ExpirationIndex<TOwnedType>,
    ) {
        if let Some(old_expires) = old_expires {
            self.do_remove(old_expires, itm.get_id_as_str());
        }

        self.add(itm)
    }

    pub fn remove(&mut self, itm: &impl ExpirationIndex<TOwnedType>) {
        let expiration_moment = itm.get_expiration_moment();

        if expiration_moment.is_none() {
            return;
        }

        self.do_remove(expiration_moment.unwrap(), itm.get_id_as_str());
    }

    fn do_remove(&mut self, expiration_moment: DateTimeAsMicroseconds, key_as_str: &str) {
        match self.find_index(expiration_moment) {
            Ok(index) => {
                let mut remove_index = None;

                if let Some(items) = self.index.get_mut(index) {
                    if items.remove(key_as_str) {
                        remove_index = Some(index);
                    }
                }

                if let Some(remove_index) = remove_index {
                    self.index.remove(remove_index);
                }

                self.amount -= 1;
            }
            Err(_) => {
                //todo!("Removed - but I have to return it");
                println!(
                    "Somehow we did not find the index for expiration moment {} of '{}'. Expiration moment as rfc3339 is {}",
                    expiration_moment.unix_microseconds, key_as_str, expiration_moment.to_rfc3339()
                );
            }
        }
    }

    pub fn get_items_to_expire<TResult>(
        &self,
        now: DateTimeAsMicroseconds,
        transform: impl Fn(&TOwnedType) -> TResult,
    ) -> Vec<TResult> {
        let mut result = Vec::new();
        for expiration_item in &self.index {
            if expiration_item.moment.unix_microseconds > now.unix_microseconds {
                break;
            }

            for itm in expiration_item.items.iter() {
                result.push(transform(itm));
            }
        }

        result
    }

    pub fn has_data_with_expiration_moment(
        &self,
        expiration_moment: DateTimeAsMicroseconds,
    ) -> bool {
        self.find_index(expiration_moment).is_ok()
    }

    pub fn len(&self) -> usize {
        self.amount
    }

    pub fn clear(&mut self) {
        self.index.clear();
    }
}

#[cfg(test)]
mod tests {
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use crate::ExpirationIndex;

    #[derive(Clone)]
    pub struct TestExpirationItem {
        pub key: String,
        pub expires: Option<DateTimeAsMicroseconds>,
    }

    impl ExpirationIndex<TestExpirationItem> for TestExpirationItem {
        fn get_id_as_str(&self) -> &str {
            &self.key
        }

        fn to_owned(&self) -> TestExpirationItem {
            self.clone()
        }

        fn get_expiration_moment(&self) -> Option<DateTimeAsMicroseconds> {
            self.expires
        }
    }

    #[cfg(test)]
    mod test {
        use rust_extensions::date_time::DateTimeAsMicroseconds;

        use crate::ExpirationIndexContainer;

        use super::TestExpirationItem;

        #[test]
        fn test_insert_expiration_key() {
            let mut index = ExpirationIndexContainer::new();

            let item = TestExpirationItem {
                key: "2".to_string(),
                expires: DateTimeAsMicroseconds::new(2).into(),
            };

            index.add(&item);

            assert_eq!(index.len(), 1);

            let item = TestExpirationItem {
                key: "1".to_string(),
                expires: DateTimeAsMicroseconds::new(1).into(),
            };

            index.add(&item);

            assert_eq!(index.len(), 2);

            assert_eq!(
                vec![1, 2],
                index
                    .index
                    .iter()
                    .map(|itm| itm.moment.unix_microseconds)
                    .collect::<Vec<_>>()
            );
        }
    }
}

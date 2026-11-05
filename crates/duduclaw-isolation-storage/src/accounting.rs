//! Admission accounting includes pending/deleting/quarantined images, not usage.
use crate::{Error, Result};

pub const MIN_RESERVE_BYTES: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Capacity { pub total: u64, pub available: u64 }
impl Capacity {
    pub fn reserve(self) -> Result<u64> {
        if self.total == 0 || self.available > self.total { return Err(Error::UnsafeBacking); }
        // Round UP to ten percent without overflowing.
        Ok(MIN_RESERVE_BYTES.max(self.total / 10 + u64::from(self.total % 10 != 0)))
    }
    pub fn admit(self, existing: impl IntoIterator<Item = u64>, additional: u64) -> Result<()> {
        let reserve = self.reserve()?;
        let sum = existing.into_iter().try_fold(additional, |sum, bytes| sum.checked_add(bytes))
            .ok_or(Error::Capacity)?;
        if sum.checked_add(reserve).ok_or(Error::Capacity)? > self.total
            || additional.checked_add(reserve).ok_or(Error::Capacity)? > self.available {
            return Err(Error::Capacity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const G: u64 = 1024 * 1024 * 1024;
    #[test]
    fn accounting_reserves_larger_of_twenty_gib_and_ten_percent() {
        assert_eq!(Capacity { total: 100 * G, available: 100 * G }.reserve(), Ok(20 * G));
        assert_eq!(Capacity { total: 300 * G + 1, available: 300 * G }.reserve(), Ok(30 * G + 1));
    }
    #[test]
    fn accounting_refuses_overcommit_low_free_and_overflow() {
        let capacity = Capacity { total: 100 * G, available: 40 * G };
        assert!(capacity.admit([60 * G], 20 * G).is_ok());
        assert_eq!(capacity.admit([60 * G + 1], 20 * G), Err(Error::Capacity));
        assert_eq!(capacity.admit([], 20 * G + 1), Err(Error::Capacity));
        assert_eq!(capacity.admit([u64::MAX], 1), Err(Error::Capacity));
        assert_eq!(Capacity { total: 0, available: 0 }.admit([], 0), Err(Error::UnsafeBacking));
        assert_eq!(Capacity { total: 1, available: 2 }.admit([], 0), Err(Error::UnsafeBacking));
    }
    #[test]
    fn growth_charges_delta_against_free_but_full_target_against_total() {
        let capacity = Capacity { total: 100 * G, available: 30 * G };
        // 50 -> 60 GiB needs 10 GiB additional allocation, not another 60.
        assert_eq!(capacity.admit([50 * G], 10 * G), Ok(()));
        assert_eq!(capacity.admit([50 * G], 10 * G + 1), Err(Error::Capacity));
        assert_eq!(Capacity { total: 100 * G, available: 60 * G }.admit([70 * G], 10 * G + 1), Err(Error::Capacity));
    }
    #[test]
    fn startup_and_runtime_keep_reserve_even_without_new_image() {
        let capacity = Capacity { total: 100 * G, available: 20 * G - 1 };
        assert_eq!(capacity.admit([50 * G], 0), Err(Error::Capacity));
    }
}

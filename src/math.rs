use num_traits::Num;
use std::ops::*;
pub fn align_down<T: Num + Not<Output = T> + BitAnd<Output = T> + Copy>(val: T, alignment: T) -> T {
    val & !(alignment - T::one())
}
pub fn align_up<T: Num + Not<Output = T> + BitAnd<Output = T> + Copy>(val: T, alignment: T) -> T {
    align_down(val + alignment - T::one(), alignment)
}

pub trait EventDataTrait: Clone + Copy + std::fmt::Debug + Default + Send + Sync + 'static {}

impl<T> EventDataTrait for T where T: Clone + Copy + std::fmt::Debug + Default + Send + Sync + 'static {}
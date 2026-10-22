trait Transform<T> {
    fn convert<U>(&self, input: U) -> T;
}
struct Worker;
impl Transform<u32> for Worker {
    fn convert<U>(&self, input: U) -> u32 {
        let local = input;
        let (left, right) = (local, local);
        left
    }
}
fn transform<T>(input: T) -> T { input }
fn invoke(value: Worker, input: u32) {
    let callback = transform::<u32>;
    let output = value.convert::<u32>(input);
    let other = Worker::convert::<u32>(&value, input);
    callback(output);
    let café = input;
	consume(café);
    // ghost local convert
    let message = "ghost local convert";
}

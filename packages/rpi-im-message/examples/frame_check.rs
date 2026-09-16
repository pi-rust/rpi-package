use feishu_sdk::ws::stream_protocol::Frame;
fn main() {
    let frame = Frame::ping(33554678);
    let bytes = frame.encode_binary();
    println!("feishu-sdk ping frame bytes: {}", hex(&bytes));
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

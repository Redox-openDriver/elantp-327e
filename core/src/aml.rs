use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_VALUE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Integer(u64),
    Buffer(Vec<u8>),
}

pub fn no_arguments() -> String {
    String::from("[]")
}

pub fn dsm_arguments(uuid: &[u8; 16], revision: u64, function: u64) -> String {
    let bytes: Vec<String> = uuid.iter().map(|b| format!("{b}")).collect();
    format!("[Buffer([{}]),Integer({revision}),Integer({function}),Package(contents:[])]", bytes.join(","))
}

fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

pub fn value(text: &str) -> Option<Value> {
    if text.len() > MAX_VALUE {
        return None;
    }
    let text = text.trim();
    if let Some(inner) = text.strip_prefix("Integer(").and_then(|rest| rest.strip_suffix(')')) {
        return number(inner).map(Value::Integer);
    }
    let inner = text.strip_prefix("Buffer(")?.strip_suffix(')')?.trim();
    let list = inner.strip_prefix('[')?.strip_suffix(']')?.trim();
    if list.is_empty() {
        return Some(Value::Buffer(Vec::new()));
    }
    let bytes: Option<Vec<u8>> = list.trim_end_matches(',').split(',').map(|item| number(item).and_then(|n| u8::try_from(n).ok())).collect();
    bytes.map(Value::Buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acpi::{HID_DSM_DESCRIPTOR_ADDRESS, HID_DSM_REVISION, HID_DSM_UUID};
    use alloc::vec;

    #[test]
    fn the_hid_dsm_call_is_written_as_acpid_reads_its_arguments() {
        let text = dsm_arguments(&HID_DSM_UUID, HID_DSM_REVISION, HID_DSM_DESCRIPTOR_ADDRESS);
        assert_eq!(text, "[Buffer([247,246,223,60,103,66,85,69,173,5,179,10,61,137,56,222]),Integer(1),Integer(1),Package(contents:[])]");
        assert_eq!(no_arguments(), "[]");
    }

    #[test]
    fn results_come_back_as_integers_or_buffers_and_anything_else_is_refused() {
        assert_eq!(value("Integer(1)"), Some(Value::Integer(1)));
        assert_eq!(value(" Integer( 0x20 ) "), Some(Value::Integer(32)));
        assert_eq!(value("Buffer([142,25,0])"), Some(Value::Buffer(vec![142, 25, 0])));
        assert_eq!(value("Buffer([ 1, 2, 3, ])"), Some(Value::Buffer(vec![1, 2, 3])));
        assert_eq!(value("Buffer([])"), Some(Value::Buffer(vec![])));
        assert_eq!(value("Buffer([256])"), None);
        assert_eq!(value("String(\"ELAN06FA\")"), None);
        assert_eq!(value("Integer(x)"), None);
        assert_eq!(value(&"Buffer([1,".repeat(20_000)), None);
    }
}

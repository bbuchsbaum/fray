use fray::server::read_frame;
use std::io::Cursor;
#[test]
fn frames_are_separate() {
    let mut r = Cursor::new(b"{}\n{}\n");
    assert_eq!(read_frame(&mut r, 10).unwrap().unwrap(), "{}\n");
    assert_eq!(read_frame(&mut r, 10).unwrap().unwrap(), "{}\n");
    assert!(read_frame(&mut r, 10).unwrap().is_none());
}
#[test]
fn oversized_frame_is_rejected() {
    assert!(read_frame(&mut Cursor::new(b"123456\n"), 6).is_err());
}
#[test]
fn incomplete_frame_is_rejected() {
    assert!(read_frame(&mut Cursor::new(b"{}"), 10).is_err());
}
#[test]
fn invalid_utf8_is_rejected() {
    assert!(read_frame(&mut Cursor::new(&[255, b'\n']), 10).is_err());
}

extern "env" fn t3_write_pty(terminal: u32, userdata: u32, data: u32, len: u32) void;

export fn ghostty_write_pty(terminal: u32, userdata: u32, data: u32, len: u32) void {
    t3_write_pty(terminal, userdata, data, len);
}

extern "env" fn t3_snapshot_read(userdata: u32, buffer: u32, capacity: u32, out_read: u32) bool;

export fn ghostty_snapshot_read(userdata: u32, buffer: u32, capacity: u32, out_read: u32) bool {
    return t3_snapshot_read(userdata, buffer, capacity, out_read);
}

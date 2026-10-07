"""Outer-PTY graphics integration scenarios."""
import fcntl
import os
import re
import shlex
import struct
import termios
import time

from terminal_loop_support import (
    BINARY,
    Session,
)

s = Session()
try:
    query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
    deadline = time.monotonic() + 8
    while query not in s.output:
        s.read()
        assert time.monotonic() < deadline, (bytes(s.output[-1000:]), s.child.poll())
    # A real outer-terminal reply must be consumed before the child shell sees
    # the next command. No image is displayed by this capability query.
    s.send(b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\nPROBE_READY\\n'\n")
    s.expect(b"\r\nPROBE_READY\r\n")
    raw = termios.tcgetattr(s.slave)
    assert not raw[3] & (termios.ECHO | termios.ICANON | termios.ISIG)
    # Pane children carry an environment marker. Interactive nested entry fails
    # before touching terminal modes and returns control to the existing shell.
    nested = shlex.quote(BINARY) + "; printf 'NESTED_STATUS:%s\\n' \"$?\"\n"
    s.send(nested.encode())
    s.expect(b"NESTED_STATUS:1")
    assert any(b"rustmux: nested Rustmux sessions are not supported" in row
               for row in s.last_rows), s.last_rows
    deadline = time.monotonic() + 3
    while not any(b"RUSTMUX_READY>" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < deadline, s.last_rows
    s.send("printf '\\n%s\\n' '中文输入'\n".encode())
    s.expect("\r\n中文输入\r\n".encode())
    s.send(b"printf '\\n%s\\n' 'backspacX\x7fe'\n")
    s.expect(b"\r\nbackspace\r\n")
    s.send(b"sleep 30\n")
    s.expect(b"sleep 30\r\n")
    time.sleep(0.1)
    s.send(b"\x03")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\nINT:%s\\n' \"$?\"\n")
    s.expect(b"\r\nINT:130\r\n")
    # A foreground process must receive the kernel's SIGWINCH and see the new size.
    s.send(b"python3 -c 'import os,signal; signal.signal(signal.SIGWINCH, lambda *_: print(\"SIZE:%s:%s\" % (os.get_terminal_size().lines, os.get_terminal_size().columns), flush=True)); print(\"WATCH_READY\", flush=True); exec(\"while True: signal.pause()\")'\n")
    s.expect(b"\r\nWATCH_READY\r\n")
    for rows, columns in [(40, 120), (18, 60), (55, 150)]:
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        s.expect(f"SIZE:{max(1, rows - 4)}:{max(1, columns - 2)}\r\n".encode())
    # Invalid transient dimensions must not terminate Rustmux or reach the child.
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
    s.read(0.15)
    assert b"SIZE:0:0" not in s.output
    assert s.child.poll() is None
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    s.expect(b"SIZE:20:78\r\n")
    s.send(b"\x03")
    s.expect(b"RUSTMUX_READY> ")
    # Output larger than the grid must still be parsed through the final marker.
    s.send(b"python3 -c 'import os; os.write(1, b\"Z\" * 200000); print(\"BURST_DONE\")'; printf '\\nLAST_OUTPUT\\n'; exit 7\n")
    s.finish(7)
    assert any(row.endswith(b"BURST_DONE") for row in s.last_rows), s.last_rows
    # Incremental frame reconstruction can retain cells beside the printed
    # marker when a large burst and session exit share the final repaint.
    assert any(row.lstrip().startswith(b"LAST_OUTPUT") for row in s.last_rows), s.last_rows
    assert b"\x1b]112\x1b\\" in s.output
    assert b"\x1b[?1049l" in s.output
finally:
    s.close()

# A supported, exactly sized outer terminal receives all three Kitty stacking
# bands, then targeted deletes after the pane clears its placements.
s = Session(pixels=(80, 24))
try:
    query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
    deadline = time.monotonic() + 8
    while query not in s.output:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    s.send(b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
    s.expect(b"RUSTMUX_READY> ")
    child_query = shlex.quote(os.path.join(os.path.dirname(__file__), "kitty_child_query.py"))
    s.send(f"python3 {child_query}\n".encode())
    s.expect(b"CHILD_GRAPHICS_REPLY_OK")
    child_file = shlex.quote(os.path.join(os.path.dirname(__file__), "kitty_child_file.py"))
    s.send(f"python3 {child_file}\n".encode())
    s.expect(b"CHILD_FILE_TRANSFER_OK")
    child_temp_file = shlex.quote(os.path.join(os.path.dirname(__file__), "kitty_child_temp_file.py"))
    s.send(f"python3 {child_temp_file}\n".encode())
    s.expect(b"CHILD_TEMP_FILE_TRANSFER_OK")
    child_shm_png = shlex.quote(os.path.join(os.path.dirname(__file__), "kitty_child_shm_png.py"))
    s.send(f"python3 {child_shm_png}\n".encode())
    # Success markers are terminal text: incremental frames may only send a
    # changed suffix while retaining earlier cells. Match reconstructed rows,
    # not a contiguous marker in the raw graphics/text transport.
    s.expect(b"CHILD_SHM_PNG_OK")
    child_upload = shlex.quote(os.path.join(os.path.dirname(__file__), "kitty_child_upload.py"))
    s.send(f"python3 {child_upload}\n".encode())
    s.expect(b"CHILD_UPLOAD_REPLIES_OK")
    child_placement = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_placement.py")
    )
    s.send(f"python3 {child_placement}\n".encode())
    s.expect(b"CHILD_PLACEMENT_REPLIES_OK")
    child_transmit_place = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_transmit_place.py")
    )
    s.send(f"python3 {child_transmit_place}\n".encode())
    s.expect(b"CHILD_TRANSMIT_PLACE_REPLIES_OK")
    child_visible_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_visible_delete.py")
    )
    s.send(f"python3 {child_visible_delete}\n".encode())
    s.expect(b"CHILD_VISIBLE_DELETE_OK")
    child_cursor_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_cursor_delete.py")
    )
    s.send(f"python3 {child_cursor_delete}\n".encode())
    s.expect(b"CHILD_CURSOR_DELETE_OK")
    child_cell_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_cell_delete.py")
    )
    s.send(f"python3 {child_cell_delete}\n".encode())
    s.expect(b"CHILD_CELL_DELETE_OK")
    child_z_cell_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_z_cell_delete.py")
    )
    s.send(f"python3 {child_z_cell_delete}\n".encode())
    s.expect(b"CHILD_Z_CELL_DELETE_OK")
    child_z_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_z_delete.py")
    )
    s.send(f"python3 {child_z_delete}\n".encode())
    s.expect(b"CHILD_Z_DELETE_OK")
    child_column_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_column_delete.py")
    )
    s.send(f"python3 {child_column_delete}\n".encode())
    s.expect(b"CHILD_COLUMN_DELETE_OK")
    child_row_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_row_delete.py")
    )
    s.send(f"python3 {child_row_delete}\n".encode())
    s.expect(b"CHILD_ROW_DELETE_OK")
    child_id_range_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_id_range_delete.py")
    )
    s.send(f"python3 {child_id_range_delete}\n".encode())
    s.expect(b"CHILD_ID_RANGE_DELETE_OK")
    child_image_number_upload = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_image_number_upload.py")
    )
    s.send(f"python3 {child_image_number_upload}\n".encode())
    s.expect(b"NUMBERED_UPLOAD_OK")
    child_numbered_placement = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_numbered_placement.py")
    )
    s.send(f"python3 {child_numbered_placement}\n".encode())
    s.expect(b"PLACE_BY_NUMBER_OK")
    child_number_delete = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_number_delete.py")
    )
    s.send(f"python3 {child_number_delete}\n".encode())
    s.expect(b"NUMBER_DELETE_OK")
    child_zlib_direct = shlex.quote(
        os.path.join(os.path.dirname(__file__), "kitty_child_zlib_direct.py")
    )
    s.send(f"python3 {child_zlib_direct}\n".encode())
    s.expect(b"ZLIB_DIRECT_OK")
    s.send(
        b"printf '\\033_Ga=T,f=32,s=1,v=1,i=5,p=1,c=1,r=1,z=-1073741825,C=1,q=2;AQIDBA==\\033\\\\"
        b"\\033_Ga=T,f=32,s=1,v=1,i=6,p=1,c=1,r=1,z=-1,C=1,q=2;AQIDBA==\\033\\\\"
        b"\\033_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1,z=0,C=1,q=2;AQIDBA==\\033\\\\'\n"
    )
    header = rb"\x1b_Ga=T,f=32,s=([0-9]+),v=([0-9]+),i=([0-9]+),z=(-2147483648|-1|0),C=1,q=2,m=0;"
    deadline = time.monotonic() + 8
    while len({int(z) for _, _, _, z in re.findall(header, s.output)}) != 3:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    uploads = re.findall(header, s.output)
    assert all(0 < int(width) <= 78 and 0 < int(height) <= 20
               for width, height, _, _ in uploads)
    uploaded = {int(z): int(image_id) for _, _, image_id, z in uploads}
    s.output.clear()
    s.send(b"printf '\\033[2J'\n")
    deletes = [f"\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\".encode()
               for image_id in uploaded.values()]
    deadline = time.monotonic() + 8
    while not all(command in s.output for command in deletes):
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

# Model operations must change the rendered screen, rather than pass through.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    # This block exercises rendering, not job control. On macOS the interactive
    # shell can emit a child-setpgid warning for short-lived stty jobs under load;
    # it overwrites the prompt and makes unrelated screen assertions fail.
    s.send(b"set +m\n")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\033[2J\\033[Habc\\033[1;2H\\033[31mX\\033[0m\\n'\n")
    s.expect(b"\r\naXc\r\n")
    assert b"\x1b[0;38;2;205;0;0;48;2;30;30;46mX" in s.last_frame, s.last_frame
    s.send(
        b"stty -echo; printf '\\n\\033[4:3;58:2::1:2:3mUNDER\\033[0m'; "
        b"stty echo; printf '\\nSTYLE_''DONE\\n'\n"
    )
    style_output = s.expect(b"\r\nSTYLE_DONE\r\n")
    assert b"\x1b[0;4:3;38;2;205;214;244;48;2;30;30;46;58;2;1;2;3mUNDER" in style_output, style_output[-2000:]
    s.send(b"printf '\\033[?1049h\\033[HALTSCREEN'; read answer; printf '\\033[?1049l'\n")
    s.expect(b"\r\nALTSCREEN\r\n")
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    assert b"aXc" in s.last_rows, s.last_rows
    # Mode 47 preserves the alternate buffer while switching back to the main
    # buffer. Split the marker literals so expect() cannot match the shell's
    # echoed command line instead of the rendered screen.
    s.send(
        b"stty -echo; printf '\\033[?47h\\033[HMODE''47'; read answer; "
        b"printf '\\033[?47l'; read answer; printf '\\033[?47h'; read answer; "
        b"printf '\\033[?47l'; stty echo; printf '\\nLEGACY_''DONE\\n'\n"
    )
    s.expect(b"MODE47")
    s.send(b"\n")
    s.expect(b"aXc")
    s.send(b"\n")
    s.expect(b"MODE47")
    s.send(b"\n")
    s.expect(b"LEGACY_DONE")
    s.send(b"printf '\\033[?25l\\nHIDDEN_CURSOR\\n'; read answer; printf '\\033[?25h'\n")
    s.expect(b"\r\nHIDDEN_CURSOR\r\n")
    assert s.last_frame.endswith(b"\x1b[?25l"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    assert s.last_frame.endswith(b"\x1b[?25h")

    # Keep header/footer fixed while LF then RI scroll only the middle rows.
    s.send(b"stty -echo; printf '\\033[2J\\033[1;1HHEADER\\033[2;1HONE\\033[3;1HTWO"
           b"\\033[4;1HTHREE\\033[5;1HFOOTER\\033[2;4r\\033[4;1H\\nSCROLLED'; "
           b"read answer; printf '\\033[2;1H\\033MREVERSED'; read answer; stty echo; printf '\\033[r\\033[6;1H'\n")
    s.expect(b"\r\nSCROLLED\r\n")
    assert s.last_rows[1:6] == [b"HEADER", b"TWO", b"THREE", b"SCROLLED", b"FOOTER"], s.last_rows
    s.send(b"\n")
    s.expect(b"\r\nREVERSED\r\n")
    assert s.last_rows[1:6] == [b"HEADER", b"REVERSED", b"TWO", b"THREE", b"FOOTER"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    for command, expected in [
        (b"L", [b"HEADER", b"ONE", b"", b"TWO", b"FOOTER"]),
        (b"M", [b"HEADER", b"ONE", b"THREE", b"", b"FOOTER"]),
        (b"S", [b"HEADER", b"TWO", b"THREE", b"", b"FOOTER"]),
        (b"T", [b"HEADER", b"", b"ONE", b"TWO", b"FOOTER"]),
    ]:
        s.send(b"stty -echo; printf '\\033[2J\\033[1;1HHEADER\\033[2;1HONE"
               b"\\033[3;1HTWO\\033[4;1HTHREE\\033[5;1HFOOTER"
               b"\\033[2;4r\\033[3;2H\\033[" + command +
               b"\\033[6;1HLINE_EDIT_DONE'; read answer; "
               b"stty echo; printf '\\033[r\\033[6;1H'\n")
        s.expect(b"\r\nLINE_EDIT_DONE\r\n")
        assert s.last_rows[1:6] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    for command, expected in [(b"@", b"ab cdefgh"), (b"P", b"abdefgh"), (b"X", b"ab defgh")]:
        s.send(b"stty -echo; printf '\\033[r\\033[2J\\033[1;1Habcdefgh"
               b"\\033[1;3H\\033[" + command +
               b"\\033[2;1HCHAR_EDIT_DONE'; read answer; "
               b"stty echo; printf '\\033[2;1H'\n")
        s.expect(b"\r\nCHAR_EDIT_DONE\r\n")
        assert s.last_rows[1] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[2;4r\\033[?6h"
           b"\\033[HORIGIN\\033[2d\\033[4GCOLUMN\\033[?6lABS"
           b"\\033[6;1HORIGIN_DONE'; read answer; "
           b"stty echo; printf '\\033[r\\033[6;1H'\n")
    s.expect(b"\r\nORIGIN_DONE\r\n")
    assert s.last_rows[1:4] == [b"ABS", b"ORIGIN", b"   COLUMN"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1Habcdefgh"
           b"\\033[1;3H\\033[4hXY\\033[4lZ"
           b"\\033[2;1HINSERT_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nINSERT_DONE\r\n")
    assert s.last_rows[1] == b"abXYZdefgh", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1HA\\033[1;80H"
           b"\\033[?7lBC\\033[?7hD\\033[3;1HWRAP_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nWRAP_DONE\r\n")
    assert s.last_rows[1:3] == [b"A" + b" " * 76 + b"C", b"D"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[3g\\033[4G\\033H"
           b"\\033[12G\\033H\\033[H\\tA\\tB\\033[ZC"
           b"\\033[2;1HTABS_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nTABS_DONE\r\n")
    assert s.last_rows[1] == b"   A       C", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033(0lqqk"
           b"\\033(B\\033[2;1H\\033)0\\016x\\017q"
           b"\\033[3;1HCHARSET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nCHARSET_DONE\r\n")
    assert s.last_rows[1:3] == ["┌──┐".encode(), "│q".encode()], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[?1049h\\033[31;44m"
           b"\\033[?25l\\033[4h\\033[3g\\033(0lqqk"
           b"\\033cRESET_DONE'; read answer; stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nRESET_DONE\r\n")
    assert s.last_rows[1] == b"RESET_DONE" and not any(s.last_rows[2:]), s.last_rows
    assert s.last_frame.endswith(b"\x1b[?25h"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033[31;44mKEPT"
           b"\\033[?25l\\033[4h\\033(0\\033[!pq"
           b"\\033[2;1HSOFT_RESET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nSOFT_RESET_DONE\r\n")
    assert s.last_rows[1] == b"KEPTq", s.last_rows
    assert s.last_frame.endswith(b"\x1b[?25h"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"exit\n")
    s.finish(0)
finally:
    s.close()

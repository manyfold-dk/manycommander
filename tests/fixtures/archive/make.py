#!/usr/bin/env python3
"""Makes the hostile archive fixtures of A-AR-2 and A-AR-3 (P3 8.2).

The files next to this script are its output, committed as they are: small archives whose
shapes exploit parser differentials and path joins. The tests list them (T2) and extract
them (T3); they never regenerate them. Run it from anywhere; it writes into its own
directory. It needs Python's standard library and the zstd, xz, bzip2, gzip and bsdtar
tools. It uses no ctypes and writes every byte on purpose: tarfile and zipfile for the
ordinary parts, struct for the crafted ones.

Fixtures (member names as stored):

  traversal.tar            ../evil, /abs/file, a/../../b, ok/file, ./dot/ok
  nul-name.tar             a pax path with a NUL byte; ok
  symlink-write-through    link -> ../outside, then link/pwned (the CVE-2025-29787 shape),
    .tar and .zip          as a tar and as a zip
  symlink-chmod.tar        d -> ../outside, then a directory member d with mode 0777 (the
                           RUSTSEC-2026-0067 shape)
  pax-size.tar             a pax size of 1024 over a ustar size of 512; the second data
                           block is a valid header of a member "smuggled" that only a
                           parser ignoring the pax size lists (the RUSTSEC-2026-0068 shape)
  hardlinks.tar            target; hard links to /etc/passwd, ../outside, missing, target
  special.tar              a character and a block device, a FIFO, a setuid file, a
                           setgid directory
  overlap.zip              two central directory entries, a and b, for one local entry
  bomb.zip                 10 MiB of zeros deflated under a declared size of 1000
  bomb.tar.zst             a member of 1 GiB of zeros, then "after"
  longname-bomb.tar.zst    a GNU long-name header that declares 64 MiB of name
  large-window.tar.zst     a zstd frame that needs a 256 MiB window
  large-dict.tar.xz        an xz block whose LZMA2 dictionary is 256 MiB
  truncated.tar.{zst,xz,gz,bz2}
                           24 members, cut at 60 % of the compressed stream
  truncated.tar            6 members, cut inside the second member's data
  encrypted.zip            a ZipCrypto entry (its encryption header is random: this one
                           file differs on every run)
"""

import io
import os
import struct
import subprocess
import tarfile
import threading
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
MTIME = 1_700_000_000


def out(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data)


def info(name, kind=tarfile.REGTYPE, size=0, mode=0o644, link=""):
    t = tarfile.TarInfo(name)
    t.type = kind
    t.size = size
    t.mode = mode
    t.mtime = MTIME
    t.uid = t.gid = 0
    t.uname = t.gname = ""
    t.linkname = link
    return t


def tar_bytes(members, fmt=tarfile.GNU_FORMAT):
    """members: (TarInfo, data or None)."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=fmt) as tf:
        for t, data in members:
            tf.addfile(t, io.BytesIO(data) if data is not None else None)
    return buf.getvalue()


def pipe(argv, data):
    return subprocess.run(argv, input=data, stdout=subprocess.PIPE, check=True).stdout


def block(data):
    return data + b"\0" * (-len(data) % 512)


def header(t):
    return t.tobuf(tarfile.USTAR_FORMAT, "utf-8", "surrogateescape")


def pax_record(key, value):
    body = b" " + key + b"=" + value + b"\n"
    n = len(body) + 1
    while len(str(n).encode()) + len(body) != n:
        n += 1
    return str(n).encode() + body


def file(name, data, mode=0o644):
    return info(name, size=len(data), mode=mode), data


# -- tar shapes ---------------------------------------------------------------------------

out(
    "traversal.tar",
    tar_bytes(
        [
            file("../evil", b"evil\n"),
            file("/abs/file", b"abs\n"),
            file("a/../../b", b"b\n"),
            file("ok/file", b"ok\n"),
            file("./dot/ok", b"dot\n"),
        ]
    ),
)

nul = info("placeholder", size=3)
nul.pax_headers = {"path": "a\0b"}
out("nul-name.tar", tar_bytes([(nul, b"nul"), file("ok", b"ok\n")], tarfile.PAX_FORMAT))

out(
    "symlink-write-through.tar",
    tar_bytes(
        [
            (info("link", tarfile.SYMTYPE, mode=0o777, link="../outside"), None),
            file("link/pwned", b"written through the link\n"),
            file("fine", b"fine\n"),
        ]
    ),
)

out(
    "symlink-chmod.tar",
    tar_bytes(
        [
            (info("d", tarfile.SYMTYPE, mode=0o777, link="../outside"), None),
            (info("d", tarfile.DIRTYPE, mode=0o777), None),
        ]
    ),
)

# pax-size.tar, byte by byte: pax(size=1024), a (ustar size 512), 1024 bytes of data whose
# second block is the header of "smuggled", then b and the end blocks.
pax_data = pax_record(b"size", b"1024")
pax = info("PaxHeaders/a", tarfile.XHDTYPE, size=len(pax_data))
a = info("a", size=512)
smuggled = info("smuggled", size=0)
b_data = b"after the pax member\n"
b_info = info("b", size=len(b_data))
raw = (
    header(pax)
    + block(pax_data)
    + header(a)
    + b"\0" * 512
    + header(smuggled)
    + header(b_info)
    + block(b_data)
    + b"\0" * 1024
)
out("pax-size.tar", raw)

out(
    "hardlinks.tar",
    tar_bytes(
        [
            file("target", b"the target\n"),
            (info("hl-abs", tarfile.LNKTYPE, link="/etc/passwd"), None),
            (info("hl-up", tarfile.LNKTYPE, link="../outside"), None),
            (info("hl-missing", tarfile.LNKTYPE, link="missing"), None),
            (info("hl-ok", tarfile.LNKTYPE, link="target"), None),
        ]
    ),
)

chr_dev = info("dev-null", tarfile.CHRTYPE, mode=0o666)
chr_dev.devmajor, chr_dev.devminor = 1, 3
blk_dev = info("dev-block", tarfile.BLKTYPE, mode=0o660)
blk_dev.devmajor, blk_dev.devminor = 8, 0
out(
    "special.tar",
    tar_bytes(
        [
            (chr_dev, None),
            (blk_dev, None),
            (info("fifo", tarfile.FIFOTYPE, mode=0o644), None),
            file("setuid", b"#!/bin/sh\n", mode=0o4755),
            (info("setgid-dir", tarfile.DIRTYPE, mode=0o2755), None),
        ]
    ),
)

# -- zip shapes ---------------------------------------------------------------------------


def zinfo(name, mode, method=zipfile.ZIP_STORED):
    z = zipfile.ZipInfo(name, date_time=(2023, 11, 14, 22, 13, 20))
    z.create_system = 3
    z.external_attr = mode << 16
    z.compress_type = method
    return z


buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as z:
    z.writestr(zinfo("link", 0o120777), "../outside")
    z.writestr(zinfo("link/pwned", 0o100644), "written through the link\n")
    z.writestr(zinfo("fine", 0o100644), "fine\n")
out("symlink-write-through.zip", buf.getvalue())


def zip_parts(data):
    """(bytes before the central directory, central records, end record)."""
    eocd = data.rindex(b"PK\x05\x06")
    count, cd_size, cd_off = struct.unpack("<HII", data[eocd + 10 : eocd + 20])
    cd = data[cd_off : cd_off + cd_size]
    records, at = [], 0
    for _ in range(count):
        n, e, c = struct.unpack("<HHH", cd[at + 28 : at + 34])
        size = 46 + n + e + c
        records.append(bytearray(cd[at : at + size]))
        at += size
    return data[:cd_off], records, bytearray(data[eocd:])


def zip_join(body, records, eocd):
    cd = b"".join(records)
    eocd[8:10] = struct.pack("<H", len(records))
    eocd[10:12] = struct.pack("<H", len(records))
    eocd[12:16] = struct.pack("<I", len(cd))
    eocd[16:20] = struct.pack("<I", len(body))
    return body + cd + bytes(eocd)


buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr(zinfo("a", 0o100644, zipfile.ZIP_DEFLATED), b"shared data\n" * 100)
body, recs, eocd = zip_parts(buf.getvalue())
twin = bytearray(recs[0])
twin[46:47] = b"b"
out("overlap.zip", zip_join(body, recs + [twin], eocd))

buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr(zinfo("bomb", 0o100644, zipfile.ZIP_DEFLATED), b"\0" * (10 << 20))
    z.writestr(zinfo("small", 0o100644), b"small\n")
data = bytearray(buf.getvalue())
body, recs, eocd = zip_parts(bytes(data))
body = bytearray(body)
struct.pack_into("<I", body, 22, 1000)  # the local header's uncompressed size
struct.pack_into("<I", recs[0], 24, 1000)  # the central directory's
out("bomb.zip", zip_join(bytes(body), recs, eocd))

enc_dir = os.path.join(HERE, ".enc")
os.makedirs(enc_dir, exist_ok=True)
with open(os.path.join(enc_dir, "secret.txt"), "wb") as f:
    f.write(b"secret data\n")
os.utime(os.path.join(enc_dir, "secret.txt"), (MTIME, MTIME))
subprocess.run(
    [
        "bsdtar",
        "--format",
        "zip",
        "--options",
        "zip:encryption=zipcrypt",
        "--passphrase",
        "fixture",
        "--uid",
        "0",
        "--gid",
        "0",
        "-C",
        enc_dir,
        "-cf",
        os.path.join(HERE, "encrypted.zip"),
        "secret.txt",
    ],
    check=True,
)
os.remove(os.path.join(enc_dir, "secret.txt"))
os.rmdir(enc_dir)

# -- compressed streams -------------------------------------------------------------------

# A member of 1 GiB of zeros, streamed into zstd, then a small member.
zs = subprocess.Popen(["zstd", "-q", "-c"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
big = info("zeros", size=1 << 30)
chunk = b"\0" * (1 << 20)


def feed():
    zs.stdin.write(header(big))
    for _ in range(1 << 10):
        zs.stdin.write(chunk)
    after = b"after the zeros\n"
    zs.stdin.write(header(info("after", size=len(after))) + block(after) + b"\0" * 1024)
    zs.stdin.close()


t = threading.Thread(target=feed)
t.start()
bomb = zs.stdout.read()
t.join()
zs.wait()
out("bomb.tar.zst", bomb)

# A GNU long-name header that declares 64 MiB of name.
long_size = 64 << 20
ln = info("././@LongLink", tarfile.GNUTYPE_LONGNAME, size=long_size)
zs = subprocess.Popen(["zstd", "-q", "-c"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)


def feed_long():
    zs.stdin.write(ln.tobuf(tarfile.GNU_FORMAT))
    for _ in range(long_size >> 20):
        zs.stdin.write(b"a" * (1 << 20))
    zs.stdin.write(header(info("x", size=0)) + b"\0" * 1024)
    zs.stdin.close()


t = threading.Thread(target=feed_long)
t.start()
longbomb = zs.stdout.read()
t.join()
zs.wait()
out("longname-bomb.tar.zst", longbomb)

small = tar_bytes([file("small", b"small\n")])
# From a pipe zstd writes the window it was asked for into the frame header.
out("large-window.tar.zst", pipe(["zstd", "--long=28", "-q", "-c"], small))

# The first block header's LZMA2 dictionary raised to 256 MiB, its CRC32 recomputed.
xz = bytearray(pipe(["xz", "-c"], small))
h_size = (xz[12] + 1) * 4
hdr = xz[12 : 12 + h_size]
at = hdr.index(b"\x21\x01")
hdr[at + 2] = 32
struct.pack_into("<I", hdr, h_size - 4, zlib.crc32(bytes(hdr[: h_size - 4])))
xz[12 : 12 + h_size] = hdr
out("large-dict.tar.xz", bytes(xz))

members = [
    file(f"m{i:02}.txt", "".join(f"member {i} line {k}\n" for k in range(400)).encode())
    for i in range(24)
]
plain = tar_bytes(members)
for ext, argv in [
    # Small blocks, so the blocks before the cut decode: a zstd block and an LZMA2 chunk
    # decode only when complete.
    ("zst", ["zstd", "-q", "-c", "--target-compressed-block-size=1024"]),
    ("xz", ["xz", "-c", "--block-size=16KiB"]),
    ("gz", ["gzip", "-n", "-c"]),
    ("bz2", ["bzip2", "-1", "-c"]),
]:
    whole = pipe(argv, plain)
    out(f"truncated.tar.{ext}", whole[: len(whole) * 6 // 10])
short = tar_bytes(members[:6])
out("truncated.tar", short[: 4 * 512 * 6 + 700])

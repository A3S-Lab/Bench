#!/usr/bin/env python3
"""A deliberately small, destination-restricted HTTP CONNECT proxy for Codex."""

from __future__ import annotations

import argparse
import fcntl
import ipaddress
import multiprocessing
import re
import selectors
import socket
import socketserver
import struct
import threading
import time
from collections.abc import Callable, Iterable
from pathlib import Path


HEADER_TIMEOUT_SECONDS = 10.0
CLIENT_HELLO_TIMEOUT_SECONDS = 10.0
CONNECT_TIMEOUT_SECONDS = 10.0
IDLE_TIMEOUT_SECONDS = 300.0
MAX_TUNNEL_LIFETIME_SECONDS = 3600.0
MAX_HEADER_BYTES = 16 * 1024
MAX_HEADER_LINES = 100
MAX_CLIENT_HELLO_BYTES = 64 * 1024
MAX_TLS_RECORD_BYTES = (1 << 14) + 256
MAX_DIRECTION_BUFFER_BYTES = 1024 * 1024
MAX_TUNNEL_BYTES = 256 * 1024 * 1024
RELAY_CHUNK_BYTES = 64 * 1024
MAX_CONNECTIONS = 24
MAX_DNS_PROCESSES = 4
MAX_RESOLVED_ADDRESSES = 16

_EXACT_HOSTS = frozenset(
    {
        "chatgpt.com",
        "ab.chatgpt.com",
        "api.openai.com",
        "auth.openai.com",
    }
)
_CONTENT_HOST = re.compile(r"^sdmntpr[a-z0-9-]+\.oaiusercontent\.com$")
_TASK_EXACT_HOSTS: frozenset[str] = frozenset()
_DNS_HOST = re.compile(
    r"^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)"
    r"(?:\.(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?))*$"
)
_DNS_SLOTS = threading.BoundedSemaphore(MAX_DNS_PROCESSES)


class ProxyRequestError(ValueError):
    """The client sent a request that this proxy must not forward."""


def validate_task_allow_host(host: str) -> str:
    """Validate one canonical exact DNS host supplied by the locked Task."""
    if (
        not host
        or len(host) > 253
        or not host.isascii()
        or host != host.lower()
        or host.endswith(".")
        or not _DNS_HOST.fullmatch(host)
    ):
        raise ProxyRequestError("Task allow host is not a canonical lowercase DNS hostname")
    try:
        ipaddress.ip_address(host)
    except ValueError:
        return host
    raise ProxyRequestError("Task allow host must not be an IP literal")


def validate_authority(authority: str) -> tuple[str, int]:
    """Validate a CONNECT authority and return its canonical host and port."""
    if not authority or any(ord(char) < 0x21 or ord(char) > 0x7E for char in authority):
        raise ProxyRequestError("invalid authority")
    if "@" in authority or "://" in authority:
        raise ProxyRequestError("userinfo and absolute URLs are forbidden")
    if authority.count(":") != 1 or authority.startswith("["):
        raise ProxyRequestError("authority must be a DNS host and port")
    host, port = authority.rsplit(":", 1)
    host = host.lower()
    if port != "443" or not host or host.endswith(".") or not _DNS_HOST.fullmatch(host):
        raise ProxyRequestError("authority is not an allowed host on port 443")
    try:
        ipaddress.ip_address(host)
    except ValueError:
        pass
    else:
        raise ProxyRequestError("IP literals are forbidden")
    if (
        host not in _EXACT_HOSTS
        and host not in _TASK_EXACT_HOSTS
        and not _CONTENT_HOST.fullmatch(host)
    ):
        raise ProxyRequestError("destination is not allowed")
    return host, 443


def parse_connect_request(header: bytes) -> tuple[str, int]:
    """Parse one complete, size-bounded HTTP CONNECT header."""
    if len(header) > MAX_HEADER_BYTES or not header.endswith(b"\r\n\r\n"):
        raise ProxyRequestError("invalid HTTP header framing")
    if any(byte not in range(0x20, 0x7F) and byte not in (0x0D, 0x0A) for byte in header):
        raise ProxyRequestError("invalid ASCII in HTTP header")
    lines = header[:-4].split(b"\r\n")
    if not lines or len(lines) > MAX_HEADER_LINES or any(not line for line in lines):
        raise ProxyRequestError("invalid HTTP header")
    try:
        request_line = lines[0].decode("ascii")
    except UnicodeDecodeError as error:
        raise ProxyRequestError("request line is not ASCII") from error
    parts = request_line.split(" ")
    if len(parts) != 3 or any(not part for part in parts):
        raise ProxyRequestError("invalid request line")
    method, authority, version = parts
    if method != "CONNECT" or version not in {"HTTP/1.0", "HTTP/1.1"}:
        raise ProxyRequestError("only HTTP CONNECT is supported")
    for line in lines[1:]:
        if b":" not in line or line.startswith((b" ", b"\t")):
            raise ProxyRequestError("invalid HTTP header")
        name, _value = line.split(b":", 1)
        valid_name_bytes = (
            b"!#$%&'*+-.^_`|~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
        )
        if not name or any(byte not in valid_name_bytes for byte in name):
            raise ProxyRequestError("invalid HTTP header name")
    return validate_authority(authority)


def default_route_interface(route_table: str) -> str:
    """Return the sole lowest-metric, active IPv4 default-route interface."""
    candidates: list[tuple[int, str]] = []
    for line in route_table.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 8 or fields[1] != "00000000" or fields[7] != "00000000":
            continue
        try:
            flags = int(fields[3], 16)
            metric = int(fields[6], 10)
        except ValueError:
            continue
        if flags & 0x1:
            candidates.append((metric, fields[0]))
    if not candidates:
        raise OSError("no active IPv4 default route")
    minimum = min(metric for metric, _interface in candidates)
    interfaces = {interface for metric, interface in candidates if metric == minimum}
    if len(interfaces) != 1:
        raise OSError("ambiguous IPv4 default route")
    return interfaces.pop()


def interface_ipv4_addresses() -> list[tuple[int, str, str]]:
    """Read one primary IPv4 address for each Linux network interface."""
    addresses: list[tuple[int, str, str]] = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        for index, name in socket.if_nameindex():
            if len(name.encode("ascii", "ignore")) > 15:
                continue
            request = struct.pack("256s", name.encode("ascii"))
            try:
                response = fcntl.ioctl(probe.fileno(), 0x8915, request)
            except OSError:
                continue
            addresses.append((index, name, socket.inet_ntoa(response[20:24])))
    return addresses


def select_internal_bind_address(
    public_interface: str, addresses: Iterable[tuple[int, str, str]]
) -> str:
    """Select exactly one private non-loopback IPv4 off the public interface."""
    candidates: list[tuple[int, str]] = []
    for index, interface, address in addresses:
        parsed = ipaddress.ip_address(address)
        if (
            interface != public_interface
            and isinstance(parsed, ipaddress.IPv4Address)
            and parsed.is_private
            and not parsed.is_loopback
            and not parsed.is_link_local
            and not parsed.is_unspecified
        ):
            candidates.append((index, parsed.compressed))
    candidates.sort()
    if len(candidates) != 1:
        raise OSError("could not identify exactly one internal IPv4 interface")
    return candidates[0][1]


def discover_internal_bind_address(route_path: Path = Path("/proc/net/route")) -> str:
    route_table = route_path.read_text(encoding="ascii")
    public_interface = default_route_interface(route_table)
    return select_internal_bind_address(public_interface, interface_ipv4_addresses())


def _resolve_worker(sender: object, host: str, port: int) -> None:
    try:
        answer = socket.getaddrinfo(
            host, port, family=socket.AF_UNSPEC, type=socket.SOCK_STREAM
        )
        sender.send((True, answer))  # type: ignore[attr-defined]
    except (OSError, ValueError):
        sender.send((False, None))  # type: ignore[attr-defined]
    finally:
        sender.close()  # type: ignore[attr-defined]


def _self_test_worker(sender: object) -> None:
    try:
        sender.send("a3s-proxy-resolver-ready")  # type: ignore[attr-defined]
    finally:
        sender.close()  # type: ignore[attr-defined]


def _spawn_result(
    worker: Callable[..., None], args: tuple[object, ...], timeout: float
) -> object:
    if not _DNS_SLOTS.acquire(timeout=timeout):
        raise TimeoutError("resolver process capacity timed out")
    receiver = None
    sender = None
    process = None
    started = False
    try:
        context = multiprocessing.get_context("spawn")
        receiver, sender = context.Pipe(duplex=False)
        process = context.Process(
            target=worker, args=(sender, *args), daemon=True
        )
        try:
            process.start()
            started = True
        except BaseException:
            started = process.pid is not None
            raise
        sender.close()
        sender = None
        if not receiver.poll(timeout):
            raise TimeoutError("resolver subprocess timed out")
        try:
            return receiver.recv()
        except EOFError as error:
            raise OSError("resolver subprocess exited without a result") from error
    finally:
        try:
            if receiver is not None:
                receiver.close()
            if sender is not None:
                sender.close()
            if process is not None:
                if started:
                    process.join(timeout=0.1)
                    if process.is_alive():
                        process.terminate()
                        process.join(timeout=1.0)
                    if process.is_alive():
                        process.kill()
                        process.join(timeout=1.0)
                    if process.is_alive():
                        raise OSError("could not terminate resolver subprocess")
                process.close()
        finally:
            _DNS_SLOTS.release()


def resolver_subprocess_self_test() -> None:
    result = _spawn_result(_self_test_worker, (), CONNECT_TIMEOUT_SECONDS)
    if result != "a3s-proxy-resolver-ready":
        raise OSError("resolver subprocess self-test failed")


def _getaddrinfo_with_timeout(host: str, port: int) -> list[tuple[object, ...]]:
    succeeded, result = _spawn_result(
        _resolve_worker, (host, port), CONNECT_TIMEOUT_SECONDS
    )
    if not succeeded:
        raise OSError("DNS resolution failed")
    return result


def resolve_global_targets(host: str, port: int) -> list[tuple[int, tuple[object, ...]]]:
    """Resolve a hostname, retaining only globally routable IP addresses."""
    targets: list[tuple[int, tuple[object, ...]]] = []
    seen: set[tuple[int, str, int]] = set()
    for family, socktype, _protocol, _canonname, sockaddr in _getaddrinfo_with_timeout(
        host, port
    ):
        if family not in (socket.AF_INET, socket.AF_INET6) or socktype != socket.SOCK_STREAM:
            continue
        address = str(sockaddr[0])
        try:
            parsed = ipaddress.ip_address(address)
        except ValueError:
            continue
        if not parsed.is_global:
            continue
        if family == socket.AF_INET:
            destination = (parsed.compressed, port)
        else:
            destination = (parsed.compressed, port, 0, 0)
        key = (family, parsed.compressed, port)
        if key not in seen:
            seen.add(key)
            targets.append((family, destination))
            if len(targets) >= MAX_RESOLVED_ADDRESSES:
                break
    if not targets:
        raise OSError("destination did not resolve to a global address")
    return targets


def connect_upstream(targets: Iterable[tuple[int, tuple[object, ...]]]) -> socket.socket:
    """Connect to the first reachable pre-resolved global address."""
    last_error: OSError | None = None
    deadline = time.monotonic() + CONNECT_TIMEOUT_SECONDS
    for family, sockaddr in targets:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        upstream = socket.socket(family, socket.SOCK_STREAM)
        upstream.settimeout(remaining)
        try:
            upstream.connect(sockaddr)
            upstream.settimeout(None)
            return upstream
        except OSError as error:
            last_error = error
            upstream.close()
    raise last_error or OSError("no upstream address was reachable")


def read_header(client: socket.socket) -> bytes:
    deadline = time.monotonic() + HEADER_TIMEOUT_SECONDS
    data = bytearray()
    while b"\r\n\r\n" not in data:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("request header timed out")
        client.settimeout(remaining)
        chunk = client.recv(min(4096, MAX_HEADER_BYTES + 1 - len(data)))
        if not chunk:
            raise ProxyRequestError("connection closed before request header")
        data.extend(chunk)
        if len(data) > MAX_HEADER_BYTES:
            raise ProxyRequestError("request header is too large")
    end = data.find(b"\r\n\r\n") + 4
    if end != len(data):
        raise ProxyRequestError("tunnel bytes before CONNECT acceptance are forbidden")
    return bytes(data)


def _read_exact(client: socket.socket, size: int, deadline: float) -> bytes:
    data = bytearray()
    while len(data) < size:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("TLS ClientHello timed out")
        client.settimeout(remaining)
        chunk = client.recv(size - len(data))
        if not chunk:
            raise ProxyRequestError("connection closed during TLS ClientHello")
        data.extend(chunk)
    return bytes(data)


def client_hello_sni(body: bytes) -> str:
    """Return the sole DNS host_name from a complete TLS ClientHello body."""
    cursor = 0

    def take(size: int) -> bytes:
        nonlocal cursor
        if size < 0 or cursor + size > len(body):
            raise ProxyRequestError("malformed TLS ClientHello")
        value = body[cursor : cursor + size]
        cursor += size
        return value

    def vector(length_bytes: int) -> bytes:
        length = int.from_bytes(take(length_bytes), "big")
        return take(length)

    take(2 + 32)
    vector(1)
    cipher_suites = vector(2)
    if not cipher_suites or len(cipher_suites) % 2:
        raise ProxyRequestError("malformed TLS cipher suites")
    if not vector(1):
        raise ProxyRequestError("malformed TLS compression methods")
    extensions = vector(2)
    if cursor != len(body):
        raise ProxyRequestError("trailing TLS ClientHello bytes")

    extension_cursor = 0
    server_name: str | None = None
    while extension_cursor < len(extensions):
        if extension_cursor + 4 > len(extensions):
            raise ProxyRequestError("malformed TLS extensions")
        kind = int.from_bytes(extensions[extension_cursor : extension_cursor + 2], "big")
        size = int.from_bytes(extensions[extension_cursor + 2 : extension_cursor + 4], "big")
        extension_cursor += 4
        end = extension_cursor + size
        if end > len(extensions):
            raise ProxyRequestError("malformed TLS extension length")
        value = extensions[extension_cursor:end]
        extension_cursor = end
        if kind != 0:
            continue
        if server_name is not None or len(value) < 2:
            raise ProxyRequestError("invalid TLS server_name extension")
        names_size = int.from_bytes(value[:2], "big")
        if names_size != len(value) - 2:
            raise ProxyRequestError("invalid TLS server_name list")
        names = value[2:]
        if len(names) < 3 or names[0] != 0:
            raise ProxyRequestError("TLS host_name is required")
        host_size = int.from_bytes(names[1:3], "big")
        if host_size != len(names) - 3:
            raise ProxyRequestError("multiple or malformed TLS server names")
        try:
            server_name = names[3:].decode("ascii").lower()
        except UnicodeDecodeError as error:
            raise ProxyRequestError("TLS SNI is not ASCII") from error
        if not _DNS_HOST.fullmatch(server_name) or server_name.endswith("."):
            raise ProxyRequestError("TLS SNI is not a canonical DNS hostname")
    if server_name is None:
        raise ProxyRequestError("TLS ClientHello has no SNI")
    return server_name


def read_authorized_client_hello(client: socket.socket, expected_host: str) -> bytes:
    """Read, validate, and retain the first TLS ClientHello record sequence."""
    deadline = time.monotonic() + CLIENT_HELLO_TIMEOUT_SECONDS
    raw = bytearray()
    handshake = bytearray()
    expected_handshake_size: int | None = None
    while expected_handshake_size is None or len(handshake) < expected_handshake_size:
        record_header = _read_exact(client, 5, deadline)
        raw.extend(record_header)
        record_size = int.from_bytes(record_header[3:5], "big")
        if (
            record_header[0] != 22
            or record_header[1] != 3
            or record_header[2] not in range(1, 5)
            or not 1 <= record_size <= MAX_TLS_RECORD_BYTES
        ):
            raise ProxyRequestError("first TLS record is not a valid handshake")
        if len(raw) + record_size > MAX_CLIENT_HELLO_BYTES:
            raise ProxyRequestError("TLS ClientHello is too large")
        payload = _read_exact(client, record_size, deadline)
        raw.extend(payload)
        handshake.extend(payload)
        if len(handshake) >= 4 and expected_handshake_size is None:
            if handshake[0] != 1:
                raise ProxyRequestError("first TLS handshake is not ClientHello")
            expected_handshake_size = 4 + int.from_bytes(handshake[1:4], "big")
            if expected_handshake_size > MAX_CLIENT_HELLO_BYTES:
                raise ProxyRequestError("TLS ClientHello is too large")
    assert expected_handshake_size is not None
    server_name = client_hello_sni(bytes(handshake[4:expected_handshake_size]))
    if server_name != expected_host:
        raise ProxyRequestError("TLS SNI does not match CONNECT authority")
    return bytes(raw)


def relay_tunnel(
    client: socket.socket, upstream: socket.socket, client_preface: bytes
) -> None:
    """Relay with bounded nonblocking buffers and no blocking writes."""
    if len(client_preface) > MAX_DIRECTION_BUFFER_BYTES:
        raise ProxyRequestError("TLS ClientHello exceeds relay buffer")
    client.setblocking(False)
    upstream.setblocking(False)
    selector = selectors.DefaultSelector()
    to_client = bytearray()
    to_upstream = bytearray(client_preface)
    client_read_open = True
    upstream_read_open = True
    client_write_shutdown = False
    upstream_write_shutdown = False
    transferred = len(client_preface)
    started = time.monotonic()
    last_activity = started

    def interests(sock: socket.socket) -> int:
        if sock is client:
            events = (
                selectors.EVENT_READ
                if client_read_open
                and len(to_upstream) < MAX_DIRECTION_BUFFER_BYTES
                and transferred < MAX_TUNNEL_BYTES
                else 0
            )
            if to_client and not client_write_shutdown:
                events |= selectors.EVENT_WRITE
            return events
        events = (
            selectors.EVENT_READ
            if upstream_read_open
            and len(to_client) < MAX_DIRECTION_BUFFER_BYTES
            and transferred < MAX_TUNNEL_BYTES
            else 0
        )
        if to_upstream and not upstream_write_shutdown:
            events |= selectors.EVENT_WRITE
        return events

    def refresh(sock: socket.socket) -> None:
        events = interests(sock)
        try:
            selector.get_key(sock)
        except KeyError:
            if events:
                selector.register(sock, events)
        else:
            if events:
                selector.modify(sock, events)
            else:
                selector.unregister(sock)

    def read_into(sock: socket.socket, output: bytearray) -> bool:
        nonlocal transferred, last_activity
        capacity = min(
            RELAY_CHUNK_BYTES,
            MAX_DIRECTION_BUFFER_BYTES - len(output),
            MAX_TUNNEL_BYTES - transferred,
        )
        if capacity <= 0:
            return False
        try:
            chunk = sock.recv(capacity)
        except (BlockingIOError, InterruptedError):
            return True
        if not chunk:
            last_activity = time.monotonic()
            return False
        output.extend(chunk)
        transferred += len(chunk)
        last_activity = time.monotonic()
        return True

    def write_from(sock: socket.socket, pending: bytearray) -> None:
        nonlocal last_activity
        try:
            sent = sock.send(pending)
        except (BlockingIOError, InterruptedError):
            return
        if sent:
            del pending[:sent]
            last_activity = time.monotonic()
    try:
        while True:
            now = time.monotonic()
            if now - started >= MAX_TUNNEL_LIFETIME_SECONDS:
                return
            if now - last_activity >= IDLE_TIMEOUT_SECONDS:
                return
            if transferred >= MAX_TUNNEL_BYTES and not to_client and not to_upstream:
                return
            if not client_read_open and not upstream_read_open and not to_client and not to_upstream:
                return

            if not client_read_open and not to_upstream and not upstream_write_shutdown:
                try:
                    upstream.shutdown(socket.SHUT_WR)
                except OSError:
                    pass
                upstream_write_shutdown = True
            if not upstream_read_open and not to_client and not client_write_shutdown:
                try:
                    client.shutdown(socket.SHUT_WR)
                except OSError:
                    pass
                client_write_shutdown = True

            refresh(client)
            refresh(upstream)
            if not selector.get_map():
                return
            timeout = min(
                IDLE_TIMEOUT_SECONDS - (now - last_activity),
                MAX_TUNNEL_LIFETIME_SECONDS - (now - started),
            )
            events = selector.select(max(0.0, timeout))
            if not events:
                return
            for key, mask in events:
                sock = key.fileobj
                if sock is client:
                    if mask & selectors.EVENT_READ and client_read_open:
                        client_read_open = read_into(client, to_upstream)
                    if mask & selectors.EVENT_WRITE and to_client:
                        write_from(client, to_client)
                else:
                    if mask & selectors.EVENT_READ and upstream_read_open:
                        upstream_read_open = read_into(upstream, to_client)
                    if mask & selectors.EVENT_WRITE and to_upstream:
                        write_from(upstream, to_upstream)
    except (ConnectionError, OSError):
        return
    finally:
        selector.close()


class ConnectProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        upstream: socket.socket | None = None
        tunnel_accepted = False
        try:
            host, port = parse_connect_request(read_header(self.request))
            self.request.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            tunnel_accepted = True
            client_hello = read_authorized_client_hello(self.request, host)
            upstream = connect_upstream(resolve_global_targets(host, port))
            relay_tunnel(self.request, upstream, client_hello)
        except ProxyRequestError:
            if not tunnel_accepted:
                self._reply(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
        except (OSError, TimeoutError):
            if not tunnel_accepted:
                self._reply(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
        finally:
            if upstream is not None:
                upstream.close()

    def _reply(self, response: bytes) -> None:
        try:
            self.request.sendall(response)
        except OSError:
            pass


class ThreadedConnectProxy(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = MAX_CONNECTIONS

    def __init__(self, server_address: tuple[str, int]) -> None:
        self._slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        super().__init__(server_address, ConnectProxyHandler)

    def process_request(self, request: socket.socket, client_address: tuple[object, ...]) -> None:
        if not self._slots.acquire(blocking=False):
            socketserver.TCPServer.shutdown_request(self, request)
            return
        super().process_request(request, client_address)

    def shutdown_request(self, request: socket.socket) -> None:
        try:
            super().shutdown_request(request)
        finally:
            self._slots.release()

    def handle_error(self, request: socket.socket, client_address: tuple[object, ...]) -> None:
        # Never print request data, headers, credentials, tunnel bytes, or tracebacks.
        del request, client_address


def main() -> None:
    global _TASK_EXACT_HOSTS
    parser = argparse.ArgumentParser(description="destination-restricted Codex CONNECT proxy")
    binding = parser.add_mutually_exclusive_group()
    binding.add_argument(
        "--listen",
        metavar="LOOPBACK_IPV4",
        help="bind a loopback IPv4 address for isolated tests only",
    )
    binding.add_argument(
        "--bind-internal",
        action="store_true",
        help="auto-bind the sole non-default-route internal IPv4 (the safe default)",
    )
    parser.add_argument("--port", type=int, default=3128)
    parser.add_argument(
        "--allow-host",
        action="append",
        default=[],
        metavar="EXACT_DNS_HOST",
        help="allow one locked Task HTTPS destination (repeatable)",
    )
    arguments = parser.parse_args()
    if not 1 <= arguments.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    try:
        _TASK_EXACT_HOSTS = frozenset(
            validate_task_allow_host(host) for host in arguments.allow_host
        )
    except ProxyRequestError as error:
        parser.error(str(error))
    if arguments.listen is not None:
        try:
            explicit = ipaddress.ip_address(arguments.listen)
        except ValueError:
            parser.error("--listen must be a loopback IPv4 address")
        if not isinstance(explicit, ipaddress.IPv4Address) or not explicit.is_loopback:
            parser.error("--listen must be a loopback IPv4 address")
        listen = explicit.compressed
    else:
        listen = discover_internal_bind_address()
    resolver_subprocess_self_test()
    with ThreadedConnectProxy((listen, arguments.port)) as server:
        server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()

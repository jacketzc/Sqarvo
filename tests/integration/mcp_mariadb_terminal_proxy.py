"""Drop transaction acknowledgments in the isolated, TLS-disabled MariaDB fixture.

The test instance listens on loopback port 53387. Point the test DSN at port 53388.
Only the terminal command names and connection closure are logged.

With --ddl, forward from loopback 53431 to 53429 and close the socket
after the server acknowledges the synthetic ddl_ack_unknown CREATE TABLE.
"""

import argparse
import asyncio
import contextlib

async def packet(reader):
    header = await reader.readexactly(4)
    payload = await reader.readexactly(int.from_bytes(header[:3], 'little'))
    return header, payload

rollback_count = 0
connection_count = 0
parser = argparse.ArgumentParser(description=__doc__)
mode = parser.add_mutually_exclusive_group()
mode.add_argument("--startup", action="store_true")
mode.add_argument("--ddl", action="store_true")
parser.add_argument("--upstream-port", type=int)
parser.add_argument("--listen-port", type=int)
arguments = parser.parse_args()
startup_mode = arguments.startup
ddl_mode = arguments.ddl
upstream_port = arguments.upstream_port or (53429 if ddl_mode else 53387)
listen_port = arguments.listen_port or (53431 if ddl_mode else 53388)


async def relay(incoming, client):
    global connection_count
    connection_count += 1
    case = connection_count
    downstream, upstream = await asyncio.open_connection('127.0.0.1', upstream_port)
    held = None

    async def commands():
        nonlocal held
        while True:
            header, payload = await packet(incoming)
            if startup_mode:
                command = (b'\x03SELECT @@transaction_isolation' if case == 1
                           else b'\x03START TRANSACTION')
                if payload == command:
                    held = 'INIT' if case == 1 else 'BEGIN'
                    print(f'case {case}: forwarded {held}', flush=True)
            elif ddl_mode:
                if payload.startswith(b'\x03CREATE TABLE '):
                    if b'`ddl_ack_unknown`' in payload:
                        held = 'DDL'
                        print('forwarded DDL', flush=True)
                    elif b'`ddl_cancel_inflight`' in payload:
                        held = 'DDL_CANCEL'
                        print('forwarded cancellable DDL', flush=True)
            elif payload in (b'\x03COMMIT', b'\x03ROLLBACK'):
                held = payload[1:].decode()
                print(f'forwarded {held}', flush=True)
            upstream.write(header + payload)
            await upstream.drain()

    async def responses():
        global rollback_count
        while True:
            header, payload = await packet(downstream)
            if held is None:
                client.write(header + payload)
                await client.drain()
            else:
                if startup_mode:
                    print(f'case {case}: withheld {held} reply packet', flush=True)
                    continue
                assert payload[0] == 0, f'{held} did not receive server OK'
                print(f'withheld {held} server OK', flush=True)
                if held == 'DDL':
                    client.close()
                    await client.wait_closed()
                    return
                if held == 'ROLLBACK':
                    rollback_count += 1
                    if rollback_count == 2:
                        print('injected disconnect before ROLLBACK acknowledgment', flush=True)
                        client.close()
                        await client.wait_closed()
                        return

    jobs = [asyncio.create_task(commands()), asyncio.create_task(responses())]
    done, waiting = await asyncio.wait(jobs, return_when=asyncio.FIRST_COMPLETED)
    for job in waiting:
        job.cancel()
    await asyncio.gather(*waiting, return_exceptions=True)
    for job in done:
        with contextlib.suppress(asyncio.IncompleteReadError, ConnectionResetError):
            job.result()
    if held:
        prefix = f'case {case}: ' if startup_mode else ''
        print(f'{prefix}closed socket after {held}', flush=True)
    upstream.close()
    client.close()

async def main():
    server = await asyncio.start_server(relay, '127.0.0.1', listen_port)
    async with server:
        await server.serve_forever()

asyncio.run(main())

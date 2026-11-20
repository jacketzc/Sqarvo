"""Drop transaction acknowledgments in the isolated, TLS-disabled MariaDB fixture.

The test instance listens on loopback port 53387. Point the test DSN at port 53388.
Only the terminal command names and connection closure are logged.
"""

import asyncio
import contextlib
import sys

async def packet(reader):
    header = await reader.readexactly(4)
    payload = await reader.readexactly(int.from_bytes(header[:3], 'little'))
    return header, payload

rollback_count = 0
connection_count = 0
startup_mode = sys.argv[1:] == ["--startup"]


async def relay(incoming, client):
    global connection_count
    connection_count += 1
    case = connection_count
    downstream, upstream = await asyncio.open_connection('127.0.0.1', 53387)
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
    server = await asyncio.start_server(relay, '127.0.0.1', 53388)
    async with server:
        await server.serve_forever()

asyncio.run(main())

"""Drop transaction acknowledgments in the isolated, TLS-disabled MariaDB fixture.

The test instance listens on loopback port 53387. Point the test DSN at port 53388.
Only the terminal command names and connection closure are logged.
"""

import asyncio
import contextlib

async def packet(reader):
    header = await reader.readexactly(4)
    payload = await reader.readexactly(int.from_bytes(header[:3], 'little'))
    return header, payload

rollback_count = 0


async def relay(incoming, client):
    downstream, upstream = await asyncio.open_connection('127.0.0.1', 53387)
    held = None

    async def commands():
        nonlocal held
        while True:
            header, payload = await packet(incoming)
            if payload in (b'\x03COMMIT', b'\x03ROLLBACK'):
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
        print(f'closed socket after {held}', flush=True)
    upstream.close()
    client.close()

async def main():
    server = await asyncio.start_server(relay, '127.0.0.1', 53388)
    async with server:
        await server.serve_forever()

asyncio.run(main())

# Registration server

[![Build Status](https://travis-ci.org/moziot/registration_server.svg?branch=master)](https://travis-ci.org/moziot/registration_server)

This server exposes a http(s) API that lets you post messages from your home network and discover them later on.

## Usage

```bash
cargo run -- -h 0.0.0.0 -p 4242 --cert-dir /etc/letsencrypt/live/knilxof.org
```

## Urls

1. /register?token=YYY&local_ip=XXX will publish `message` to other clients who also connect from the same outgoing IP address as you.
2. /info?token=YYY will return a json representation of the record associated to this token.
3. /subscribe?name=XXX&desc=description will reserve the name `XXX` if it doesn't exist yet and create a token for it.
4. /unsubscribe?token=YYY will delete the record for this token.
5. /dnsconfig?challenge=XXX&token=YYY configures the dns server for this domain.
6. /ping returns the dns names of the servers registered from the same network.
7. /adddiscovery?token=XXX&disco=YYY adds a discovery token bound to this domain token.
8. /revokediscovery?token=XXX&disco=YYY revokes a discovery token.
9. /discovery?disco=XXX returns the best url available to reach a server from this app's token.

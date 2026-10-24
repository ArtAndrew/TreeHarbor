Synthetic public inputs; the fixture secret is not a credential. HMAC signatures
were generated independently with Python's standard-library `hmac`/`hashlib`, over
the exact UTF-8 body strings, including their final newline. The normalized JSON
is a manually specified contract, not output captured from the Rust parser.
GitHub's separate published HMAC test vector is exercised in the library test:
https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
These fixtures are not evidence from an installed GitHub App.

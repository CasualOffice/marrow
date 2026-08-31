# Session service

Issues and validates session credentials. Two token kinds: a short lived
access credential and a long lived one used only to obtain the first.

Rotation is mandatory. The reuse detector is the reason it is mandatory, and
the tests in token_refresh cover both the happy path and the replay.

Configuration lives in the deployment manifest, not here.

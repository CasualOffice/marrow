# Deploying the service (2023)

SUPERSEDED. This procedure was retired in March 2024. Follow the current
deploy runbook instead. It is kept only because two old release notes link
to it.

## Steps
1. Stop every worker.
2. Apply migrations by hand from the bastion host.
3. Start the workers again and watch the log for exceptions.
4. Announce the deploy in the channel.

## Rolling back
Rolling back means redeploying the previous tag and reversing the migration
by hand, which has never worked cleanly.

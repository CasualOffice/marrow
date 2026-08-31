# Deploying the service

Current procedure. Supersedes the 2023 runbook.

## Before you start
Confirm the release tag is signed, that the migration plan has been reviewed,
and that nobody else holds the deploy lock.

## Steps
1. Take a snapshot of the primary database.
2. Apply migrations with the guarded runner; it refuses to run if the schema
   version does not match the tag.
3. Roll the workers one availability zone at a time, waiting for the health
   endpoint to go green before the next zone.
4. Flip the router weight to the new version in ten percent increments.

## Rolling back
Rolling back is a router weight change, not a redeploy. Migrations are
forward-only and every one of them must be safe to leave in place.

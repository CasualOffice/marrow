# Database failover

## Who to call
The on-call rota for the database is in the schedule named db-primary. The
secondary escalation is the platform lead. Do not page the application team
first; they cannot promote a replica.

## Promoting a replica
Check replication lag first. Promotion with more than thirty seconds of lag
loses writes, and the writes it loses are the ones that were in flight during
the incident.

Fence the old primary before promoting. A split brain here is worse than ten
extra minutes of downtime.

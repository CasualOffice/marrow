# Security

Personal open-source project. No SLA, no bounty, no coordinated-disclosure process. Report anything you find as a normal issue.

## What this software does to your machine

It reads every file in the folders you grant it and stores derived text, structure and metadata in a local SQLite database. From M5 it can also modify files.

**Don't point it at anything you wouldn't want an LLM to read.**

## The threat that actually matters here

Indirect prompt injection. A hostile PDF, a cloned repo's README, or text inside a screenshot can contain instructions aimed at an AI agent. Once this index is wired to a write-capable agent, the blast radius is your home directory.

The design assumes this and defends against it structurally, not by asking the model nicely:

- Retrieved content is serialized into labelled untrusted blocks with runtime-generated delimiters, never concatenated into a system prompt
- Enforcement is independent of whether the model complies — the policy layer blocks the action regardless
- Content the agent itself wrote is marked and barred from supporting claims
- Model-proposed commands are shown as resolved, literal argv before execution — never as a model-authored summary

An adversarial test corpus covers these paths and must pass before any write tool ships. It only grows.

Details: [Part 1 §6](docs/Part_1_Master_Specification.md), [Part 6 §114](docs/Part_6_Engineering_Reference.md), [Part 7 §126](docs/Part_7_Solo_Rescope.md).

## What this project deliberately does not have

- **An OS sandbox.** Shell execution runs with the invoking user's privileges. This is a single-operator tool and the author already runs arbitrary shell. Do not treat it as an isolation boundary ([Part 7 §129](docs/Part_7_Solo_Rescope.md))
- **Multi-user isolation beyond OS file permissions.** Anyone with your account has your index
- **Encryption at rest**, unless you enable it explicitly

Stating this plainly matters more than the features themselves: never claim protection you don't have.

## Credentials

API keys go in the OS keyring — never in `settings.json`, never in logs, never in the database. There's a test for this.

## Data that never leaves the machine

Nothing phones home. No telemetry, no crash reporting, no licence check. The only network traffic is what you configure: model downloads, and inference requests to a provider you supplied a key for — those go directly from your machine to that provider under your own account.

# Universal Action Protocol v0.1

Action envelope fields:
- `type=ACTION`
- `version`
- `action`
- `action_id`
- `timestamp`
- `sender`
- optional `recipient`
- `payload`
- `signature`

Action set:
- `SPEAK`
- `WHISPER`
- `OFFER` / `ACCEPT`
- `TASK_REQUEST` / `TASK_RESULT`

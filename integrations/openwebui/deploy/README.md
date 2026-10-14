# Open WebUI + rpi deployment

Use the pinned official Open WebUI slim image through the domestic NJU GHCR mirror
(`ghcr.nju.edu.cn/open-webui/open-webui:v0.12.0-slim`), the prebuilt Linux `rpi` binary,
and locally built `rpi-acp-bridge`, `librpi_ask_user.so`, and
`librpi_permission_gate.so`. No Rust compiler is needed on the server.

Place this compose file in a dedicated service directory with:

- `runtime/`: executable `rpi` and `rpi-acp-bridge`
- `extensions/`: the two extension libraries
- `config/`: private `models.json`, optional `auth.json`, and `settings.json`
- `workspace/`: projects the agent may work on
- `data/`: persistent users, chats, ACP sessions and logs
- `.env`: a fixed random `WEBUI_SECRET_KEY`, `WEBUI_URL`, and optional port/bind address

Create the external `app-net` network if absent. Start with `docker compose up -d`.
Create the first administrator with a temporary `ENABLE_SIGNUP=true` override,
then disable signup and restart. Never commit credentials or the service data.

Install `../rpi_acp_pipe.py` as an active Open WebUI Pipe Function. Set its valves:

```json
{
  "ACP_COMMAND": "/opt/rpi/runtime/rpi-acp-bridge",
  "ACP_ARGS": "[]",
  "WORKSPACE": "/workspace/rpi-package",
  "STATE_DIR": "/app/backend/data/rpi/openwebui"
}
```

Set `defaultProvider` and `defaultModel` in the private rpi settings file.
Choose **RPI (ACP)** in the model selector. Permission prompts and `ask_user`
questions appear in the browser; the rpi process runs inside the container.

Health check: `GET /health`. Check real agent connectivity with one short prompt,
then test an `ask_user` question and a tool permission. Keep the service behind
authenticated access; the agent's workspace mount controls which project files
it can edit.

The accompanying Taskfile manages the deployed instance on **bigfish-server**
(`192.168.3.41`, EasyTier `10.100.100.8`), independently of fnOS. Run
`task -t integrations/openwebui/deploy/Taskfile.yml health` or `status` from the
repository. The default service directory is `/home/bigfish/work/rpi-webui` and
the browser port is `5231`. The deployment uses SQLite in its persistent data
directory; it does not provision another database server.

Public URL: `https://rpi-webui.laofu.online`. The accompanying Caddy site goes
in `/root/work/ai-infr/config/caddy/sites/` on the existing cloud gateway and
proxies to `10.100.100.8:5231`. Set `WEBUI_URL` to the public URL in the private
environment file. Validate the complete Caddy configuration before reloading.

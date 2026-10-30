"""Validate actual v2 mapper output against pinned official schemas.

Requires jsonschema>=4. Run after generating fixtures:
  RPI_ACP_V2_FIXTURES=<file> cargo test -p rpi-acp --test v2_semantics ...
  python packages/rpi-acp/tests/v2_schema_validation.py --fixtures <file> --schema-dir <pinned-dir>

Schema validation cannot prove Git patch semantics: semantic tests separately
assert omission when native fragments do not supply a full file snapshot.
"""
import argparse
import json
from pathlib import Path

from jsonschema import Draft202012Validator


def validator(schema, definition):
    return Draft202012Validator({"$defs": schema["$defs"], "$ref": f"#/$defs/{definition}"})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema-dir", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    args = parser.parse_args()
    v1 = json.loads((args.schema_dir / "schema-v1.json").read_text(encoding="utf-8"))
    v2 = json.loads((args.schema_dir / "schema-v2.json").read_text(encoding="utf-8"))
    fixtures = json.loads(args.fixtures.read_text(encoding="utf-8"))
    for definition, value in fixtures:
        validator(v2, definition).validate(value)

    # Negative controls prove we are resolving official discriminated variants,
    # not merely accepting every object or the schemas' future-variant fallback.
    invalid = [
        ("InitializeResponse", {"protocolVersion": 2, "agentInfo": {"name": "v1"}}),
        ("InitializeRequest", {"protocolVersion": 65536, "info": {"name": "x", "version": "1"}}),
        ("InitializeRequest", {"protocolVersion": 2, "info": {"name": "x", "version": "1"},
                               "capabilities": {"auth": {"terminal": True}}}),
        ("AuthMethod", {"type": "terminal", "id": "v1", "name": "Terminal"}),
        ("AuthMethod", {"type": "terminal", "methodId": "id", "name": "Terminal", "env": {"A": "B"}}),
        ("LoginAuthRequest", {"methodId": 42}),
        ("SessionUpdate", {"sessionUpdate": "tool_call_update", "content": []}),
        ("SessionUpdate", {"sessionUpdate": "tool_call_update", "toolCallId": "t",
                           "content": [{"type": "diff", "path": "/work/a", "newText": "v1"}]}),
        ("SessionUpdate", {"sessionUpdate": "plan_update", "entries": []}),
        ("SessionUpdate", {"sessionUpdate": "state_update", "state": {"type": "idle"}}),
        ("SessionUpdate", {"sessionUpdate": "state_update", "state": None}),
        ("SessionUpdate", {"sessionUpdate": "state_update"}),
        ("ReplayFrom", {"type": 12}),
    ]
    for definition, value in invalid:
        assert not validator(v2, definition).is_valid(value), (definition, value)

    # Cross-version control: genuine v1 diffs are still valid v1, not v2.
    legacy_diff = {"path": "/work/a", "oldText": "", "newText": "hello"}
    validator(v1, "Diff").validate(legacy_diff)
    assert not validator(v2, "Diff").is_valid(legacy_diff)
    # Do not mistake schema acceptance for implementation support: future
    # cursors are schema-valid but MUST be rejected by the replay parser.
    validator(v2, "ReplayFrom").validate({"type": "_future"})
    print(f"PASS: {len(fixtures)} mapper fixtures, {len(invalid)} negative controls, v1/v2 separation")


if __name__ == "__main__":
    main()

-- A config that already exists, with no keys anywhere: no credentials page (a first start is
-- told by the config folder being new). Runs in welcome's home, where welcome left both a
-- config and no setup-done.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")

T.check("an existing config starts on a chat", serpis.buffer.kind(), "chat")
T.check("not on the credentials page", serpis.buffer.name(), "*chat*")
T.finish()

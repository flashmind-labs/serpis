-- Model presets: commands that switch the current chat's model and thinking level (a port of
-- the pi extension). Copy to ~/.config/serpis/extensions/.
--
--   heavy   GLM 5.3 (openrouter/z-ai/glm-5.3) with thinking "max"
--   light   GLM 5.3 Flash (openrouter/z-ai/glm-5.3-flash) with thinking "high"
--   extra   Kimi K3 (openrouter/moonshotai/kimi-k3) with thinking "high"
--
-- They change this chat only, not the default for new chats (agent-model). The presets are a
-- setting, so init.lua can replace them: serpis.var.set("model-presets", {...}).

serpis.var.define("model-presets", {
  heavy = { model = "openrouter/z-ai/glm-5.3", label = "GLM 5.3 (thinking max)", thinking = "max" },
  light = { model = "openrouter/z-ai/glm-5.3-flash", label = "GLM 5.3 Flash (thinking high)", thinking = "high" },
  extra = { model = "openrouter/moonshotai/kimi-k3", label = "Kimi K3 (thinking high)", thinking = "high" },
}, "Commands that switch the current chat's model: {name = {model = \"provider/id\", label, thinking}}.")

for name, p in pairs(serpis.var.get("model-presets")) do
  serpis.command.define(name, function()
    local m = serpis.agent.find_model(p.model)
    if not m then
      serpis.error(string.format('Model "%s" not found in the registry.', p.model))
    elseif not m.configured then
      serpis.error(string.format('Authentication not configured for provider "%s".', m.provider))
    else
      serpis.chat.set_model(nil, p.model)
      serpis.chat.set_thinking(nil, p.thinking)
      serpis.message("model: " .. p.label)
    end
  end, "Switch this chat to " .. p.label .. ".")
end

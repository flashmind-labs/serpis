-- Model presets: switch the current chat's model and thinking level in one go (a port of the
-- pi extension). Copy to ~/.config/serpis/extensions/.
--
--   M-x presets   pick one from a list
--   heavy         GLM 5.3 (openrouter/z-ai/glm-5.3) with thinking "max"
--   light         GLM 5.3 Flash (openrouter/z-ai/glm-5.3-flash) with thinking "high"
--   extra         Kimi K3 (openrouter/moonshotai/kimi-k3) with thinking "high"
--
-- They change this chat only, not the default for new chats (agent-model). The presets are a
-- setting, so init.lua can replace them: serpis.var.set("model-presets", {...}).

serpis.var.define("model-presets", {
  heavy = { model = "openrouter/z-ai/glm-5.3", label = "GLM 5.3 (thinking max)", thinking = "max" },
  light = { model = "openrouter/z-ai/glm-5.3-flash", label = "GLM 5.3 Flash (thinking high)", thinking = "high" },
  extra = { model = "openrouter/moonshotai/kimi-k3", label = "Kimi K3 (thinking high)", thinking = "high" },
}, "Commands that switch the current chat's model: {name = {model = \"provider/id\", label, thinking}}.")

local function apply(p)
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
end

local presets = serpis.var.get("model-presets")
for name, p in pairs(presets) do
  serpis.command.define(name, function()
    apply(p)
  end, "Switch this chat to " .. p.label .. ".")
end

serpis.command.define("presets", function()
  local current = serpis.chat.info()
  local names = {}
  for name in pairs(presets) do table.insert(names, name) end
  table.sort(names)
  local candidates = {}
  for _, name in ipairs(names) do
    local p = presets[name]
    local on = current.model == p.model and current.thinking == p.thinking
    table.insert(candidates, { label = name, value = name, detail = p.label .. (on and "  ✓" or "") })
  end
  serpis.minibuffer.read({
    prompt = "Preset: ",
    candidates = candidates,
    require_match = true,
    sort = false,
    on_accept = function(r)
      if r.value then apply(presets[r.value]) end
    end,
  })
end, "Choose a model preset for this chat from a list.")

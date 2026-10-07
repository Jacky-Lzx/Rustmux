-- Isolated configuration: enable the installed plugin, without capability overrides.
vim.opt.rtp:prepend(vim.env.RUSTMUX_COMPAT_SNACKS)
vim.opt.termguicolors = true
local Snacks = require("snacks")
Snacks.config.image = {
  enabled = true,
  force = false,
  doc = { enabled = false },
  cache = vim.env.PROBE_DIR .. "/cache",
}
local terminal = Snacks.image.terminal
local requests = {}
local request = terminal.request
terminal.request = function(opts)
  local fields = vim.deepcopy(opts)
  fields.data = fields.data and ("payload:" .. #fields.data) or nil
  table.insert(requests, fields)
  return request(opts)
end
local function save(info)
  vim.fn.writefile({ vim.json.encode(info) }, vim.env.PROBE_DIR .. "/result.json")
end
vim.api.nvim_create_autocmd("VimEnter", {
  once = true,
  callback = function()
    terminal.detect(function(term)
      local success, err = xpcall(function()
        local info = {
          terminal = term,
          env = terminal.env(),
          size = terminal.size(),
          supported = Snacks.image.supports_terminal(),
        }
        local ok, placement = pcall(Snacks.image.buf._attach, vim.api.nvim_get_current_buf(), {
          src = vim.env.PROBE_IMAGE,
          width = 12,
          height = 6,
        })
        info.error = not ok and tostring(placement) or nil
        vim.defer_fn(function()
          info.requests = requests
          save(info)
          vim.cmd("redraw")
        end, 2500)
      end, debug.traceback)
      if not success then
        save({ error = err })
      end
    end)
  end,
})

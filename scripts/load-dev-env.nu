# 在项目根目录 source 此文件；沿用可信的 .dev.env，不打印凭据或原始错误。
# 仅导入 Chromie 相关变量，保留 Nushell 自己的 PATH 与配置。
let chromie_env_file = ($env.CHROMIE_ENV_FILE? | default ($env.PWD | path join '.dev.env'))
let chromie_process = (
    ^sh -c 'set -ae; . "$1"; python3 -c "import json,os; print(json.dumps({k:v for k,v in os.environ.items() if k.startswith(\"CHROMIE_\") or k == \"DATABASE_URL\"}))"' sh $chromie_env_file
    | complete
)
if $chromie_process.exit_code != 0 { error make {msg: '无法加载开发环境，请在本地检查 .dev.env；未输出配置内容。'} }
let chromie_values = (try { $chromie_process.stdout | from json } catch { error make {msg: '开发环境输出格式无效；未输出配置内容。'} })
load-env $chromie_values

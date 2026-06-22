git init

touch README.md
git add README.md
git commit -m "initialize"

h5i init
h5i hook setup --write --wrap-bash --team
git add .
git commit -m "setup hook"

git branch implement
git switch implement

h5i env create claude-1 --profile agent-claude
h5i env create codex-1 --profile agent-codex

echo "Implement Quick Sort from scratch in Python. We also need to provide enough pytest unit tests" > TASK.md

h5i team create demo
h5i team add-env demo env/human/claude-1 --runtime claude
h5i team add-env demo env/human/codex-1 --runtime codex

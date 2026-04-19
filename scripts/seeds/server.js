const express = require("express");
const app = express();
app.use(express.json());

const users = new Map();

app.post("/register", (req, res) => {
  const { username, password } = req.body;
  if (users.has(username)) return res.status(409).json({ error: "user exists" });
  users.set(username, password);
  res.status(201).json({ username });
});

app.post("/login", (req, res) => {
  const { username, password } = req.body;
  if (users.get(username) !== password)
    return res.status(401).json({ error: "invalid credentials" });
  res.json({ token: Buffer.from(username).toString("base64") });
});

app.get("/profile/:username", (req, res) => {
  if (!users.has(req.params.username))
    return res.status(404).json({ error: "not found" });
  res.json({ username: req.params.username });
});

app.listen(3000, () => console.log("listening on :3000"));

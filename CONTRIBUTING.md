# Contributing

Thanks for your interest in `ctx`.

## Ground rules

- Open an issue before starting on anything larger than a small fix, so the direction can
  be agreed first.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, optional scope such as `feat(registry):`).
- Every change to the security rules or the publish API needs a test that runs against
  the Firebase emulators. Access control is enforced only there.
- Do not commit Firebase project ids, API keys or service-account files. The repository
  must stay deployable to any Firebase project.

## Running the tests

```sh
cd registry/functions
npm install
npm test
```

## Security issues

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).

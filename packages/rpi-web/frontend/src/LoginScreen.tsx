import { useRef, useState, type FormEvent } from "react";
import {
  ArrowRight,
  Code2,
  Eye,
  EyeOff,
  Languages,
  Loader2,
  Moon,
  ShieldCheck,
  Sun,
} from "lucide-react";
import { translate, type Language } from "./i18n";

type Props = {
  checking: boolean;
  accessError?: boolean;
  onRetry?: () => void;
  language: Language;
  theme: "light" | "dark";
  onLanguage: (value: Language) => void;
  onTheme: (value: "light" | "dark") => void;
  password: string;
  onPassword: (value: string) => void;
  busy: boolean;
  error: string;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
};

export function LoginScreen(props: Props) {
  const t = (key: Parameters<typeof translate>[1]) =>
    translate(props.language, key);
  const [visible, setVisible] = useState(false);
  const [capsLock, setCapsLock] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const error =
    props.error === "authentication failed"
      ? t("loginRejected")
      : props.error
        ? t("loginFailed")
        : "";

  return (
    <main className="login-screen">
      <header className="login-header">
        <div className="login-brand">
          <Code2 size={19} aria-hidden="true" />
          <span>rpi</span>
          <span className="login-brand-divider" />
          <span className="login-workspace-label">{t("workspace")}</span>
        </div>
        <div className="login-preferences">
          <label className="login-language">
            <Languages size={16} aria-hidden="true" />
            <select
              aria-label={t("language")}
              value={props.language}
              onChange={(event) =>
                props.onLanguage(event.target.value as Language)
              }
            >
              <option value="zh">简体中文</option>
              <option value="en">English</option>
            </select>
          </label>
          <button
            type="button"
            className="login-theme"
            aria-label={t(
              props.theme === "light" ? "switchToDark" : "switchToLight",
            )}
            onClick={() =>
              props.onTheme(props.theme === "light" ? "dark" : "light")
            }
          >
            {props.theme === "light" ? <Moon size={17} /> : <Sun size={17} />}
          </button>
        </div>
      </header>
      <section className="login-center" aria-labelledby="login-title">
        <div className="login-panel">
          <div className="login-mark" aria-hidden="true">
            <Code2 size={28} strokeWidth={1.7} />
          </div>
          <h1 id="login-title">
            {t(props.checking ? "connectingTitle" : "signIn")}
          </h1>
          <p className="login-intro">{t("loginIntro")}</p>
          {props.checking ? (
            <p className="login-checking" role="status">
              <Loader2 size={17} className="login-spinner" aria-hidden="true" />
              {t("checkingAccess")}
            </p>
          ) : props.accessError ? (
            <div className="login-form">
              <p role="alert">{t("accessCheckFailed")}</p>
              <button
                className="login-submit"
                type="button"
                onClick={props.onRetry}
              >
                {t("retryAccess")}
              </button>
            </div>
          ) : (
            <form
              className="login-form"
              onSubmit={props.onSubmit}
              aria-busy={props.busy}
            >
              <label htmlFor="workspace-password">
                {t("workspacePassword")}
              </label>
              <div className="login-password">
                <input
                  ref={input}
                  id="workspace-password"
                  name="password"
                  type={visible ? "text" : "password"}
                  autoComplete="current-password"
                  autoCapitalize="none"
                  spellCheck={false}
                  required
                  disabled={props.busy}
                  value={props.password}
                  onChange={(event) => props.onPassword(event.target.value)}
                  onKeyDown={(event) =>
                    setCapsLock(event.getModifierState("CapsLock"))
                  }
                  onKeyUp={(event) =>
                    setCapsLock(event.getModifierState("CapsLock"))
                  }
                  onBlur={() => setCapsLock(false)}
                  aria-invalid={!!error}
                  aria-describedby={`login-hint${error ? " login-error" : ""}${capsLock ? " login-caps" : ""}`}
                  autoFocus
                />
                <button
                  type="button"
                  className="login-reveal"
                  aria-label={t(visible ? "hidePassword" : "showPassword")}
                  aria-pressed={visible}
                  disabled={props.busy}
                  onClick={() => {
                    setVisible(!visible);
                    input.current?.focus();
                  }}
                >
                  {visible ? <EyeOff size={18} /> : <Eye size={18} />}
                </button>
              </div>
              <p id="login-hint" className="login-hint">
                {t("passwordHint")}
              </p>
              <div
                className="login-feedback"
                aria-live="polite"
                aria-atomic="true"
              >
                {error ? (
                  <p id="login-error" role="alert">
                    {error}
                  </p>
                ) : capsLock ? (
                  <p id="login-caps">{t("capsLockOn")}</p>
                ) : null}
              </div>
              <button
                type="submit"
                className="login-submit"
                disabled={props.busy || !props.password.trim()}
              >
                {props.busy ? (
                  <Loader2
                    size={17}
                    className="login-spinner"
                    aria-hidden="true"
                  />
                ) : null}
                {t(props.busy ? "signingIn" : "signInButton")}
                {!props.busy && <ArrowRight size={17} aria-hidden="true" />}
              </button>
            </form>
          )}
        </div>
      </section>
      <footer className="login-footer">
        <ShieldCheck size={14} aria-hidden="true" />
        {t("loopback")}
      </footer>
    </main>
  );
}

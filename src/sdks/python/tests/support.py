from email.message import EmailMessage
import os
from pathlib import Path
import smtplib
import ssl


def inbox_options():
    certs = Path(__file__).resolve().parents[4] / ".dev" / "certs"
    return {
        "server_url": os.environ.get("AHRARA_TEST_SERVER_URL", "wss://localhost:8443"),
        "server_public_key": os.environ.get("AHRARA_TEST_SERVER_PUBLIC_KEY")
        or (certs / "transport-public.txt").read_text().strip(),
        "tls_ca_path": os.environ.get("AHRARA_TEST_TLS_CA", str(certs / "ca.pem")),
        "connect_timeout_ms": 10000,
    }


def send_email(
    address,
    subject,
    body="Verification code: 123456",
    *,
    html=None,
    attachment=None,
    headers=None,
):
    host, port = os.environ.get("AHRARA_TEST_SMTP_ADDRESS", "localhost:2525").rsplit(
        ":", 1
    )
    message = EmailMessage()
    message["From"] = "sender@example.test"
    message["To"] = address
    message["Subject"] = subject

    for name, value in (headers or {}).items():
        message[name] = value

    message.set_content(body)
    if html is not None:
        message.add_alternative(html, subtype="html")
    if attachment is not None:
        message.add_attachment(
            attachment,
            maintype="application",
            subtype="octet-stream",
            filename="sample.bin",
        )

    # Trust only the test CA; PROTOCOL_TLS_CLIENT verifies both peer and hostname.
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.load_verify_locations(cafile=inbox_options()["tls_ca_path"])
    with smtplib.SMTP(host, int(port), timeout=10) as smtp:
        smtp.starttls(context=context)
        smtp.send_message(message)


def get_variables():
    options = inbox_options()
    return {
        "AHRARA_URL": options["server_url"],
        "AHRARA_PIN": options["server_public_key"],
        "AHRARA_CA": options["tls_ca_path"],
    }

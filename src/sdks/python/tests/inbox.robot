*** Settings ***
Library    support.py
Variables    support.py
Library    ahrara.robot.AhraraLibrary    ${AHRARA_URL}    ${AHRARA_PIN}    ${AHRARA_CA}    WITH NAME    Ahrara
Test Teardown    Ahrara.Close Inbox

*** Test Cases ***
Receive A Real Email Without A CLI
    ${address}=    Ahrara.Create Inbox
    Send Email    ${address}    Robot confirmation    Verification code: 123456
    ${email}=    Ahrara.Wait For Email    timeout=10    subject=Robot confirmation
    Should Be Equal    ${email}[subject]    Robot confirmation
    Should Contain    ${email}[text_body]    123456
    ${page}=    Ahrara.List Emails
    Length Should Be    ${page}[emails]    1
    ${stored}=    Ahrara.Get Email    ${email}[id]
    Should Be Equal    ${stored}[subject]    Robot confirmation
    ${raw}=    Ahrara.Get Raw Email    ${email}[id]
    Should Not Be Empty    ${raw}
    Run Keyword And Expect Error    *No matching email*    Ahrara.Wait For Email    timeout=0.1
    ${deleted}=    Ahrara.Delete Email    ${email}[id]
    Should Be Equal As Integers    ${deleted}    1
    ${deleted}=    Ahrara.Clear Emails
    Should Be Equal As Integers    ${deleted}    0

Closing An Inbox Is Idempotent
    Ahrara.Create Inbox
    Ahrara.Close Inbox
    Ahrara.Close Inbox

Inspect An Attachment And HTML
    ${address}=    Ahrara.Create Inbox
    ${bytes}=    Evaluate    bytes([0, 1, 2, 255])
    Send Email    ${address}    Robot rich mail    Plain text    html=<p>Robot HTML</p>    attachment=${bytes}
    ${email}=    Ahrara.Wait For Email    timeout=10
    ${attachment}=    Ahrara.Get Attachment    ${email}[id]    ${email}[attachments][0][attachment_id]
    Should Be Equal    ${attachment}[content]    ${bytes}
    ${html}=    Ahrara.Get Html    ${email}[id]
    Should Contain    ${html}[html]    Robot HTML

Extract Captures And XPath Values
    ${address}=    Ahrara.Create Inbox
    Send Email    ${address}    Robot regex    Code: 987654
    ${match}=    Ahrara.Wait For Regex Match    (?<code>[0-9]+)    timeout=10
    Should Be Equal    ${match}[value]    987654
    Should Be Equal    ${match}[groups][1][name]    code
    Send Email    ${address}    Robot XPath    fallback    html=<a href="/verify">Verify</a>
    ${value}=    Ahrara.Wait For XPath    //a/@href    timeout=10
    Should Be Equal    ${value}    /verify

Filter With A Regex Dictionary
    ${address}=    Ahrara.Create Inbox
    Send Email    ${address}    Robot filter skipped    Ignore me
    Send Email    ${address}    Robot filter 42    Use me
    &{filter}=    Create Dictionary    regex=${TRUE}    subject=^Robot filter [0-9]+$
    ${email}=    Ahrara.Wait For Email    timeout=10    filter=${filter}
    Should Be Equal    ${email}[subject]    Robot filter 42

describe("Ahrara signup", () => {
  let address;
  afterEach(() => {
    if (address) cy.task("closeInbox", address);
  });
  it("receives email through the native SDK in cy.task", () => {
    cy.task("createInbox").then((value) => {
      address = value;
      cy.visit("/");
      cy.get("[name=email]").type(address);
      cy.contains("button", "Sign up").click();
      cy.task("waitForRegexMatch", address).then((match) => {
        const code = match.groups.find((group) => group.name === "code").value;
        expect(code).to.equal("123456");
        cy.get("#status").should("have.text", "Email sent");
        cy.get("[name=code]").type(code);
        cy.get("#verify").click();
        cy.get("#status").should("have.text", "Verified");
      });
    });
  });
});

# Keyword Extraction Patterns

Extended synonym lists and NLP patterns for ATS matching.

## Table of Contents

1. [Technology Synonyms](#technology-synonyms)
2. [Role & Title Synonyms](#role--title-synonyms)
3. [Soft Skill Patterns](#soft-skill-patterns)
4. [Industry Terms](#industry-terms)
5. [Extraction Regex Patterns](#extraction-regex-patterns)

## Technology Synonyms

### Programming Languages

| Canonical | Synonyms |
|-----------|----------|
| JavaScript | JS, ECMAScript, ES5, ES6, ES6+, ES2015+, Vanilla JS |
| TypeScript | TS, TSX |
| Python | Python2, Python3, Py, Py3 |
| Java | J2EE, JDK, JRE, Java SE, Java EE |
| C# | CSharp, C-Sharp, .NET C# |
| C++ | CPP, C Plus Plus |
| Go | Golang |
| Ruby | RoR (with Rails) |
| Rust | Rust-lang |
| PHP | PHP7, PHP8 |
| Kotlin | KT |
| Swift | SwiftUI (includes Swift) |
| Scala | Scala 2, Scala 3 |

### Frontend Frameworks

| Canonical | Synonyms |
|-----------|----------|
| React | React.js, ReactJS, React 18, React Hooks |
| Vue | Vue.js, VueJS, Vue 2, Vue 3 |
| Angular | AngularJS, Angular 2+, Angular 15+ |
| Next.js | NextJS, Next |
| Svelte | SvelteKit |
| jQuery | JQuery, jQuery UI |

### Backend Frameworks

| Canonical | Synonyms |
|-----------|----------|
| Node.js | Node, NodeJS, Express, Express.js |
| Django | Django REST, DRF |
| Flask | Flask-RESTful |
| Spring | Spring Boot, Spring MVC, Spring Framework |
| .NET | .NET Core, .NET Framework, ASP.NET |
| Ruby on Rails | Rails, RoR |
| FastAPI | Fast API |
| NestJS | Nest.js, Nest |

### Databases

| Canonical | Synonyms |
|-----------|----------|
| PostgreSQL | Postgres, PSQL, PG |
| MySQL | MariaDB (partial), MySQL 8 |
| MongoDB | Mongo, MongoDB Atlas |
| Redis | Redis Cache, Redis Cluster |
| Elasticsearch | ES, Elastic, ELK |
| DynamoDB | Dynamo, AWS DynamoDB |
| Cassandra | Apache Cassandra |
| SQL Server | MSSQL, MS SQL, Microsoft SQL Server |
| Oracle | Oracle DB, Oracle Database |
| SQLite | SQLite3 |

### Cloud & Infrastructure

| Canonical | Synonyms |
|-----------|----------|
| Amazon Web Services | AWS, Amazon Cloud |
| Google Cloud Platform | GCP, Google Cloud |
| Microsoft Azure | Azure, MS Azure |
| Kubernetes | K8s, K8, Kube |
| Docker | Docker Compose, Containerization |
| Terraform | TF, HashiCorp Terraform, IaC |
| Ansible | Ansible Playbooks |
| Jenkins | Jenkins CI, Jenkins Pipeline |
| GitHub Actions | GHA, GitHub CI/CD |
| GitLab CI | GitLab CI/CD, GitLab Pipelines |
| CircleCI | Circle CI |
| AWS Lambda | Lambda, Serverless (partial) |
| EC2 | AWS EC2, Elastic Compute |
| S3 | AWS S3, Simple Storage Service |
| CloudFormation | CFN, AWS CFN |

### DevOps & Practices

| Canonical | Synonyms |
|-----------|----------|
| CI/CD | Continuous Integration, Continuous Deployment, Continuous Delivery |
| Infrastructure as Code | IaC, Infra as Code |
| Site Reliability Engineering | SRE |
| DevOps | DevSecOps (includes DevOps) |
| GitOps | Git-based Operations |
| Microservices | Micro-services, MSA |
| Serverless | FaaS, Lambda Architecture |
| Agile | Scrum, Kanban, XP, Agile/Scrum |
| Test-Driven Development | TDD |
| Behavior-Driven Development | BDD |

### Data & ML

| Canonical | Synonyms |
|-----------|----------|
| Machine Learning | ML, Statistical Learning |
| Deep Learning | DL, Neural Networks |
| Artificial Intelligence | AI, AI/ML |
| Natural Language Processing | NLP |
| Computer Vision | CV, Image Recognition |
| TensorFlow | TF (context: ML) |
| PyTorch | Torch |
| Pandas | Python Pandas |
| NumPy | Numpy |
| Scikit-learn | sklearn, scikit |
| Apache Spark | Spark, PySpark |
| Hadoop | Apache Hadoop, HDFS |
| Kafka | Apache Kafka |
| Airflow | Apache Airflow |

### APIs & Protocols

| Canonical | Synonyms |
|-----------|----------|
| REST API | RESTful, REST, RESTful API |
| GraphQL | GQL, Graph QL |
| gRPC | GRPC, Google RPC |
| WebSocket | WebSockets, WS |
| OAuth | OAuth 2.0, OAuth2 |
| JWT | JSON Web Token |
| OpenAPI | Swagger, OAS |

## Role & Title Synonyms

### Engineering Leadership

| Canonical | Equivalents |
|-----------|-------------|
| Head of Engineering | VP Engineering, Engineering VP, SVP Engineering |
| Engineering Director | Director of Engineering, Senior Director Engineering |
| Engineering Manager | EM, Dev Manager, Development Manager |
| Tech Lead | Technical Lead, Lead Engineer, Team Lead |
| Staff Engineer | Staff Software Engineer, Senior Staff |
| Principal Engineer | Principal Software Engineer, Distinguished Engineer |
| Architect | Software Architect, Solutions Architect, Technical Architect |
| CTO | Chief Technology Officer, Chief Technical Officer |

### Product & Design

| Canonical | Equivalents |
|-----------|-------------|
| Product Manager | PM, Product Owner, PO |
| Product Director | Director of Product, Head of Product |
| UX Designer | User Experience Designer, Product Designer |
| UI Designer | User Interface Designer, Visual Designer |
| Design Lead | Lead Designer, Senior Designer |

### Data Roles

| Canonical | Equivalents |
|-----------|-------------|
| Data Engineer | Data Platform Engineer, Analytics Engineer |
| Data Scientist | ML Engineer (partial), Research Scientist |
| Data Analyst | Business Analyst, Analytics Analyst |
| ML Engineer | Machine Learning Engineer, AI Engineer |

## Soft Skill Patterns

### Leadership Indicators

**Keywords**: led, managed, directed, oversaw, headed, supervised, mentored, coached

**Patterns**:
- "Led a team of [X]"
- "Managed [X] engineers"
- "Directed [X] initiatives"
- "Mentored [X] junior"
- "Built and scaled team from [X] to [Y]"

### Communication Indicators

**Keywords**: presented, communicated, collaborated, stakeholder, cross-functional

**Patterns**:
- "Presented to [executives/board/leadership]"
- "Communicated with stakeholders"
- "Cross-functional collaboration"
- "Partnered with [team/department]"

### Problem-Solving Indicators

**Keywords**: resolved, solved, improved, optimized, reduced, increased, achieved

**Patterns**:
- "Resolved [X] issue"
- "Improved [metric] by [X]%"
- "Reduced [cost/time/errors] by [X]%"
- "Optimized [system/process]"

### Innovation Indicators

**Keywords**: introduced, pioneered, first, innovative, novel, created, designed

**Patterns**:
- "Introduced [new technology/process]"
- "Pioneered [approach/methodology]"
- "First to implement [X]"
- "Designed and built [X] from scratch"

### Strategic Thinking Indicators

**Keywords**: strategy, roadmap, vision, long-term, planning, initiative

**Patterns**:
- "Developed [X] strategy"
- "Created technical roadmap"
- "Defined vision for [X]"
- "Led strategic initiative"

## Industry Terms

### E-commerce

| Terms |
|-------|
| conversion rate, cart abandonment, checkout flow, product catalog |
| inventory management, order fulfillment, SKU, PDP, PLP |
| personalization, recommendation engine, A/B testing |
| payment processing, fraud detection, customer journey |

### FinTech

| Terms |
|-------|
| payment processing, transaction, settlement, clearing |
| KYC, AML, compliance, regulatory, PCI DSS |
| trading, portfolio, risk management, underwriting |
| banking API, open banking, neobank |

### Healthcare

| Terms |
|-------|
| HIPAA, PHI, EHR, EMR, HL7, FHIR |
| clinical, patient, provider, payer |
| telehealth, telemedicine, digital health |
| FDA, medical device, diagnostics |

### SaaS / Enterprise

| Terms |
|-------|
| multi-tenant, SLA, uptime, scalability |
| enterprise, B2B, self-service, subscription |
| onboarding, retention, churn, ARR, MRR |
| SSO, SAML, RBAC, IAM |

## Extraction Regex Patterns

### Years of Experience

```regex
(\d+)\+?\s*(?:years?|yrs?)\s*(?:of)?\s*(?:experience|exp)?
```

Examples matched:
- "5+ years of experience"
- "3 years experience"
- "10 yrs exp"

### Team Size

```regex
(?:team of|managed|led)\s*(\d+)(?:\+)?\s*(?:engineers?|developers?|people|members?)?
```

Examples matched:
- "team of 10 engineers"
- "managed 5+ developers"
- "led 20 people"

### Budget/Revenue

```regex
\$?\s*(\d+(?:\.\d+)?)\s*(?:M|B|K|million|billion|thousand)
```

Examples matched:
- "$5M"
- "10 million"
- "$2.5B"

### Percentages/Metrics

```regex
(\d+(?:\.\d+)?)\s*%\s*(?:increase|decrease|improvement|reduction|growth)?
```

Examples matched:
- "50% increase"
- "25% reduction"
- "99.9% uptime"

### Degree Extraction

```regex
(?:Bachelor'?s?|Master'?s?|PhD|Ph\.D\.|MBA|BS|BA|MS|MA)\s*(?:in|of)?\s*([A-Za-z\s]+)?
```

Examples matched:
- "Bachelor's in Computer Science"
- "MS Computer Engineering"
- "MBA"
- "PhD in Machine Learning"

### Certification Extraction

```regex
(?:AWS|Google|Azure|Cisco|PMP|Scrum|Certified|Certificate)\s*[A-Za-z\s\-]+(?:Certified|Certificate|Professional|Associate|Expert)?
```

Examples matched:
- "AWS Solutions Architect Certified"
- "PMP Certified"
- "Google Cloud Professional"
- "Certified Scrum Master"

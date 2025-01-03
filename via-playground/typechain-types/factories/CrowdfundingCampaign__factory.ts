/* Autogenerated file. Do not edit manually. */
/* tslint:disable */
/* eslint-disable */
import { Contract, ContractFactory, ContractTransactionResponse, Interface } from 'ethers';
import type { Signer, BigNumberish, ContractDeployTransaction, ContractRunner } from 'ethers';
import type { NonPayableOverrides } from '../common';
import type { CrowdfundingCampaign, CrowdfundingCampaignInterface } from '../CrowdfundingCampaign';

const _abi = [
    {
        inputs: [
            {
                internalType: 'uint256',
                name: '_fundingGoal',
                type: 'uint256'
            }
        ],
        stateMutability: 'nonpayable',
        type: 'constructor'
    },
    {
        anonymous: false,
        inputs: [
            {
                indexed: false,
                internalType: 'address',
                name: 'contributor',
                type: 'address'
            },
            {
                indexed: false,
                internalType: 'uint256',
                name: 'amount',
                type: 'uint256'
            }
        ],
        name: 'ContributionReceived',
        type: 'event'
    },
    {
        anonymous: false,
        inputs: [
            {
                indexed: false,
                internalType: 'uint256',
                name: 'totalFundsRaised',
                type: 'uint256'
            }
        ],
        name: 'GoalReached',
        type: 'event'
    },
    {
        inputs: [],
        name: 'contribute',
        outputs: [],
        stateMutability: 'payable',
        type: 'function'
    },
    {
        inputs: [],
        name: 'getFundingGoal',
        outputs: [
            {
                internalType: 'uint256',
                name: '',
                type: 'uint256'
            }
        ],
        stateMutability: 'view',
        type: 'function'
    },
    {
        inputs: [],
        name: 'getTotalFundsRaised',
        outputs: [
            {
                internalType: 'uint256',
                name: '',
                type: 'uint256'
            }
        ],
        stateMutability: 'view',
        type: 'function'
    },
    {
        inputs: [],
        name: 'owner',
        outputs: [
            {
                internalType: 'address',
                name: '',
                type: 'address'
            }
        ],
        stateMutability: 'view',
        type: 'function'
    },
    {
        inputs: [],
        name: 'withdrawFunds',
        outputs: [],
        stateMutability: 'nonpayable',
        type: 'function'
    }
] as const;

const _bytecode =
    '0x6080604052348015600f57600080fd5b5060405161089b38038061089b8339818101604052810190602f919060b1565b336000806101000a81548173ffffffffffffffffffffffffffffffffffffffff021916908373ffffffffffffffffffffffffffffffffffffffff160217905550806001819055505060d9565b600080fd5b6000819050919050565b6091816080565b8114609b57600080fd5b50565b60008151905060ab81608a565b92915050565b60006020828403121560c45760c3607b565b5b600060d084828501609e565b91505092915050565b6107b3806100e86000396000f3fe60806040526004361061004a5760003560e01c80630c3e2d2d1461004f57806324600fc31461007a5780638da5cb5b14610091578063b85090f3146100bc578063d7bb99ba146100e7575b600080fd5b34801561005b57600080fd5b506100646100f1565b6040516100719190610427565b60405180910390f35b34801561008657600080fd5b5061008f6100fb565b005b34801561009d57600080fd5b506100a66102ae565b6040516100b39190610483565b60405180910390f35b3480156100c857600080fd5b506100d16102d2565b6040516100de9190610427565b60405180910390f35b6100ef6102dc565b005b6000600254905090565b60008054906101000a900473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff1614610189576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161018090610521565b60405180910390fd5b60015460025410156101d0576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016101c79061058d565b60405180910390fd5b6000479050600060028190555060008060009054906101000a900473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1682604051610224906105de565b60006040518083038185875af1925050503d8060008114610261576040519150601f19603f3d011682016040523d82523d6000602084013e610266565b606091505b50509050806102aa576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016102a19061063f565b60405180910390fd5b5050565b60008054906101000a900473ffffffffffffffffffffffffffffffffffffffff1681565b6000600154905090565b6000341161031f576040517f08c379a0000000000000000000000000000000000000000000000000000000008152600401610316906106d1565b60405180910390fd5b34600360003373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff168152602001908152602001600020600082825461036e9190610720565b9250508190555034600260008282546103879190610720565b925050819055507f1bb460ccaaf70fbacfec17a376f8acbd278c1405590ffcc8ebe4b88daf4f64ad33346040516103bf929190610754565b60405180910390a16001546002541061040c577ffbfd8ab7c24300fa9888cd721c8565a7da56759384781283684dcf7c7c4a846b6002546040516104039190610427565b60405180910390a15b565b6000819050919050565b6104218161040e565b82525050565b600060208201905061043c6000830184610418565b92915050565b600073ffffffffffffffffffffffffffffffffffffffff82169050919050565b600061046d82610442565b9050919050565b61047d81610462565b82525050565b60006020820190506104986000830184610474565b92915050565b600082825260208201905092915050565b7f4f6e6c7920746865206f776e65722063616e2077697468647261772066756e6460008201527f7300000000000000000000000000000000000000000000000000000000000000602082015250565b600061050b60218361049e565b9150610516826104af565b604082019050919050565b6000602082019050818103600083015261053a816104fe565b9050919050565b7f46756e64696e6720676f616c206e6f7420726561636865640000000000000000600082015250565b600061057760188361049e565b915061058282610541565b602082019050919050565b600060208201905081810360008301526105a68161056a565b9050919050565b600081905092915050565b50565b60006105c86000836105ad565b91506105d3826105b8565b600082019050919050565b60006105e9826105bb565b9150819050919050565b7f5472616e73666572206661696c65642e00000000000000000000000000000000600082015250565b600061062960108361049e565b9150610634826105f3565b602082019050919050565b600060208201905081810360008301526106588161061c565b9050919050565b7f436f6e747269627574696f6e206d75737420626520677265617465722074686160008201527f6e20300000000000000000000000000000000000000000000000000000000000602082015250565b60006106bb60238361049e565b91506106c68261065f565b604082019050919050565b600060208201905081810360008301526106ea816106ae565b9050919050565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052601160045260246000fd5b600061072b8261040e565b91506107368361040e565b925082820190508082111561074e5761074d6106f1565b5b92915050565b60006040820190506107696000830185610474565b6107766020830184610418565b939250505056fea2646970667358221220b75d7e62023227b156afefc30a299466017c1a319a898bc6fe71c794aaa1d7bc64736f6c634300081b0033';

type CrowdfundingCampaignConstructorParams = [signer?: Signer] | ConstructorParameters<typeof ContractFactory>;

const isSuperArgs = (xs: CrowdfundingCampaignConstructorParams): xs is ConstructorParameters<typeof ContractFactory> =>
    xs.length > 1;

export class CrowdfundingCampaign__factory extends ContractFactory {
    constructor(...args: CrowdfundingCampaignConstructorParams) {
        if (isSuperArgs(args)) {
            super(...args);
        } else {
            super(_abi, _bytecode, args[0]);
        }
    }

    override getDeployTransaction(
        _fundingGoal: BigNumberish,
        overrides?: NonPayableOverrides & { from?: string }
    ): Promise<ContractDeployTransaction> {
        return super.getDeployTransaction(_fundingGoal, overrides || {});
    }
    override deploy(_fundingGoal: BigNumberish, overrides?: NonPayableOverrides & { from?: string }) {
        return super.deploy(_fundingGoal, overrides || {}) as Promise<
            CrowdfundingCampaign & {
                deploymentTransaction(): ContractTransactionResponse;
            }
        >;
    }
    override connect(runner: ContractRunner | null): CrowdfundingCampaign__factory {
        return super.connect(runner) as CrowdfundingCampaign__factory;
    }

    static readonly bytecode = _bytecode;
    static readonly abi = _abi;
    static createInterface(): CrowdfundingCampaignInterface {
        return new Interface(_abi) as CrowdfundingCampaignInterface;
    }
    static connect(address: string, runner?: ContractRunner | null): CrowdfundingCampaign {
        return new Contract(address, _abi, runner) as unknown as CrowdfundingCampaign;
    }
}
